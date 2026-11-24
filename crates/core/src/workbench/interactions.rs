//! Save metrics returned while the user opens a selected article in WeChat.
use serde::{Deserialize, Serialize};
#[cfg(any(windows, test))]
use serde_json::Value;

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct ArticleInteractions {
    pub read_count: Option<i64>,
    pub like_count: Option<i64>,
    pub watching_count: Option<i64>,
    pub comment_count: Option<i64>,
}

#[cfg(any(windows, test))]
impl ArticleInteractions {
    fn parse(payload: &Value) -> Option<Self> {
        let obj = payload.as_object()?;
        for ret in [obj.get("ret"), payload.pointer("/base_resp/ret")]
            .into_iter()
            .flatten()
        {
            if count(Some(ret)) != Some(0) {
                return None;
            }
        }
        let stats = payload.get("appmsgstat").unwrap_or(&Value::Null);
        let metrics = Self {
            read_count: count(stats.get("read_num")),
            like_count: count(stats.get("old_like_num")),
            watching_count: count(stats.get("like_num")),
            comment_count: count(payload.get("comment_count"))
                .or_else(|| count(stats.get("comment_count"))),
        };
        (metrics != Self::default()).then_some(metrics)
    }

    fn parse_html(html: &str) -> Option<Self> {
        use std::sync::LazyLock;

        static ARTICLE_CONTENT: LazyLock<regex::Regex> = LazyLock::new(|| {
            regex::Regex::new(r#"(?i)<[a-z][a-z0-9]*[^>]*\sid\s*=\s*["']js_content["']"#).unwrap()
        });
        static SCRIPTS: LazyLock<regex::Regex> =
            LazyLock::new(|| regex::Regex::new(r"(?is)<script\b[^>]*>(.*?)</script\s*>").unwrap());
        static COUNTS: LazyLock<regex::Regex> = LazyLock::new(|| {
            regex::Regex::new(
                r#"\b(read_num|old_like_num|like_num|comment_count)\b\\?["']?\s*[:=]\s*\\?["']?([0-9]+)\b"#,
            )
            .unwrap()
        });
        if !ARTICLE_CONTENT.is_match(html) {
            return None;
        }

        let mut values = [None; 4];
        let mut ambiguous = [false; 4];
        for script in SCRIPTS.captures_iter(html) {
            let source = script.get(1)?.as_str();
            for entry in COUNTS.captures_iter(source) {
                let index = match &entry[1] {
                    "read_num" => 0,
                    "old_like_num" => 1,
                    "like_num" => 2,
                    "comment_count" => 3,
                    _ => continue,
                };
                if let Ok(value) = entry[2].parse::<i64>() {
                    ambiguous[index] |= values[index].is_some_and(|prior| prior != value);
                    values[index] = Some(value);
                }
            }
        }
        for (value, is_ambiguous) in values.iter_mut().zip(ambiguous) {
            if is_ambiguous {
                *value = None;
            }
        }
        let [read_count, like_count, watching_count, comment_count] = values;
        let metrics = Self {
            read_count,
            like_count,
            watching_count,
            comment_count,
        };
        (metrics != Self::default()).then_some(metrics)
    }
}

#[cfg(any(windows, test))]
fn count(value: Option<&Value>) -> Option<i64> {
    let value = value?;
    value
        .as_i64()
        .or_else(|| value.as_str()?.parse().ok())
        .filter(|number| *number >= 0)
}

#[cfg(any(windows, test))]
mod capture {
    use std::sync::{Arc, Mutex};

    use anyhow::{Context, Result};
    use http_body_util::BodyExt;
    use hudsucker::hyper::{header, Method, Request, Response};
    use hudsucker::rustls::server::ClientHello;
    use hudsucker::{decode_response, Body, HttpContext, HttpHandler, RequestOrResponse};
    use tokio::sync::oneshot;

    use super::ArticleInteractions;

    const HOST: &str = "mp.weixin.qq.com";
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum ArticleResponse {
        Page,
        Stats,
    }

    impl ArticleResponse {
        fn max_bytes(self) -> usize {
            match self {
                Self::Page => 6 * 1024 * 1024,
                Self::Stats => 1024 * 1024,
            }
        }

        fn parse(self, body: &[u8]) -> Option<ArticleInteractions> {
            match self {
                Self::Page => ArticleInteractions::parse_html(std::str::from_utf8(body).ok()?),
                Self::Stats => serde_json::from_slice(body)
                    .ok()
                    .as_ref()
                    .and_then(ArticleInteractions::parse),
            }
        }
    }

    #[derive(Clone)]
    pub(super) struct ArticleIdentity {
        pub biz: String,
        pub mid: String,
        pub idx: i64,
    }

    impl ArticleIdentity {
        fn response_kind(&self, req: &Request<Body>) -> Option<ArticleResponse> {
            let kind = match (req.method(), req.uri().path()) {
                (&Method::GET, "/s") => ArticleResponse::Page,
                (&Method::POST, "/mp/getappmsgext") => ArticleResponse::Stats,
                _ => return None,
            };
            if req
                .uri()
                .host()
                .or_else(|| req.headers().get(header::HOST)?.to_str().ok())
                != Some(HOST)
                || !req
                    .headers()
                    .get(header::USER_AGENT)
                    .and_then(|value| value.to_str().ok())
                    .is_some_and(|ua| ua.contains("MicroMessenger"))
            {
                return None;
            }
            let mut biz = false;
            let mut mid = false;
            let mut idx = false;
            for (key, value) in
                url::form_urlencoded::parse(req.uri().query().unwrap_or("").as_bytes())
            {
                match key.as_ref() {
                    "__biz" if !biz && value == self.biz => biz = true,
                    "mid" if !mid && value == self.mid => mid = true,
                    "idx" if !idx && value.parse::<i64>() == Ok(self.idx) => idx = true,
                    "__biz" | "mid" | "idx" => return None,
                    _ => {}
                }
            }
            (biz && mid && idx).then_some(kind)
        }
    }

    type Sender = oneshot::Sender<ArticleInteractions>;

    #[derive(Clone)]
    pub(super) struct Handler {
        target: Arc<ArticleIdentity>,
        sender: Arc<Mutex<Option<Sender>>>,
        response_kind: Option<ArticleResponse>,
    }

    impl Handler {
        pub fn new(target: ArticleIdentity, sender: Sender) -> Self {
            Self {
                target: Arc::new(target),
                sender: Arc::new(Mutex::new(Some(sender))),
                response_kind: None,
            }
        }

        async fn observe(&self, response: Response<Body>, kind: ArticleResponse) -> Response<Body> {
            use hudsucker::hyper::body::Body as _;
            let limit = kind.max_bytes();
            if !response.status().is_success() || response.body().size_hint().lower() > limit as u64
            {
                return response;
            }

            let (parts, body) = response.into_parts();
            let Ok(body) = body.collect().await else {
                return Response::from_parts(parts, Body::empty());
            };
            let bytes = body.to_bytes();
            if bytes.len() <= limit {
                let mut copy = Response::new(Body::from(bytes.clone()));
                *copy.headers_mut() = parts.headers.clone();
                if let Ok(decoded) = decode_response(copy) {
                    if let Ok(body) = http_body_util::Limited::new(decoded.into_body(), limit)
                        .collect()
                        .await
                    {
                        if let Some(metrics) = kind.parse(&body.to_bytes()) {
                            if let Some(sender) = self.sender.lock().unwrap().take() {
                                let _ = sender.send(metrics);
                            }
                        }
                    }
                }
            }
            Response::from_parts(parts, Body::from(bytes))
        }
    }

    impl HttpHandler for Handler {
        async fn should_intercept_connect(&mut self, _: &HttpContext, req: &Request<Body>) -> bool {
            req.uri().host() == Some(HOST)
        }

        async fn should_intercept_tls(&mut self, _: &HttpContext, hello: ClientHello<'_>) -> bool {
            hello.server_name() == Some(HOST)
        }

        async fn handle_request(
            &mut self,
            _: &HttpContext,
            request: Request<Body>,
        ) -> RequestOrResponse {
            let waiting = self
                .sender
                .lock()
                .unwrap()
                .as_ref()
                .is_some_and(|sender| !sender.is_closed());
            self.response_kind = if waiting {
                self.target.response_kind(&request)
            } else {
                None
            };
            request.into()
        }

        async fn handle_response(
            &mut self,
            _: &HttpContext,
            response: Response<Body>,
        ) -> Response<Body> {
            if let Some(kind) = self.response_kind {
                return self.observe(response, kind).await;
            }
            response
        }
    }

    pub(super) async fn run<G>(
        ca: crate::ca::CaMaterial,
        target: ArticleIdentity,
        install_proxy: impl FnOnce(u16) -> std::result::Result<G, String>,
        ready: impl FnOnce(),
    ) -> Result<ArticleInteractions> {
        use hudsucker::rustls::crypto::aws_lc_rs;
        use hudsucker::Proxy;
        use std::time::Duration;

        let _guard = crate::runstate::try_acquire_run().context("已有代理采集任务在运行")?;
        crate::capture::install_default_crypto();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let port = listener.local_addr()?.port();
        let (sender, result) = oneshot::channel();
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let proxy = Proxy::builder()
            .with_listener(listener)
            .with_ca(ca.authority()?)
            .with_rustls_connector(aws_lc_rs::default_provider())
            .with_http_handler(Handler::new(target, sender))
            .with_graceful_shutdown(async move {
                let _ = shutdown_rx.await;
            })
            .build()?;
        let mut proxy = tokio::spawn(proxy.start());
        let proxy_guard = match install_proxy(port) {
            Ok(guard) => guard,
            Err(error) => {
                proxy.abort();
                anyhow::bail!("无法启用本机代理：{error}");
            }
        };
        ready();
        let result = tokio::time::timeout(Duration::from_secs(60), result).await;
        drop(proxy_guard);
        // Let the just-forwarded article response finish before aborting the listener.
        let _ = shutdown_tx.send(());
        if tokio::time::timeout(Duration::from_secs(2), &mut proxy)
            .await
            .is_err()
        {
            proxy.abort();
        }
        result
            .context("60 秒内未收到该文章的互动数据。请在微信内打开或刷新原文后重试")?
            .context("互动响应监听已结束")
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use serde_json::json;

        #[tokio::test]
        async fn only_selected_article_and_valid_counts_are_accepted() {
            let target = ArticleIdentity {
                biz: "B==".into(),
                mid: "42".into(),
                idx: 2,
            };
            let request = |query: &str| {
                Request::post(format!("https://{HOST}/mp/getappmsgext?{query}"))
                    .header(header::USER_AGENT, "MicroMessenger")
                    .body(Body::empty())
                    .unwrap()
            };
            assert_eq!(
                target.response_kind(&request("__biz=B%3D%3D&mid=42&idx=2")),
                Some(ArticleResponse::Stats)
            );
            assert_eq!(
                target.response_kind(&request("__biz=B%3D%3D&mid=42&idx=1")),
                None
            );
            assert_eq!(
                target.response_kind(&request("__biz=B%3D%3D&mid=42&idx=2&idx=2")),
                None
            );

            let (sender, mut receiver) = oneshot::channel();
            let handler = Handler::new(target, sender);
            let body = json!({"base_resp":{"ret":-1},"comment_count":0}).to_string();
            handler
                .observe(Response::new(Body::from(body)), ArticleResponse::Stats)
                .await;
            assert!(receiver.try_recv().is_err());
            let body = json!({"appmsgstat":{"old_like_num":"8"},"comment_count":0}).to_string();
            let returned = handler
                .observe(
                    Response::new(Body::from(body.clone())),
                    ArticleResponse::Stats,
                )
                .await;
            assert_eq!(
                returned
                    .into_body()
                    .collect()
                    .await
                    .unwrap()
                    .to_bytes()
                    .as_ref(),
                body.as_bytes()
            );
            let counts = receiver.await.unwrap();
            assert_eq!(counts.comment_count, Some(0));
            assert_eq!(counts.like_count, Some(8));
            assert_eq!(counts.read_count, None);
        }

        #[tokio::test]
        async fn selected_article_page_returns_current_embedded_counts() {
            let target = ArticleIdentity {
                biz: "B==".into(),
                mid: "42".into(),
                idx: 2,
            };
            let request = Request::get("https://mp.weixin.qq.com/s?__biz=B%3D%3D&mid=42&idx=2")
                .header(header::USER_AGENT, "MicroMessenger")
                .body(Body::empty())
                .unwrap();
            assert_eq!(target.response_kind(&request), Some(ArticleResponse::Page));
            let (sender, receiver) = oneshot::channel();
            let handler = Handler::new(target, sender);
            let page = r#"<div id="js_content">comment_count: 999</div>
                <script>window.counts = {"read_num":97794,"comment_count":54};</script>
                <script>other = {read_num:97794,comment_count:54};</script>"#;
            let returned = handler
                .observe(Response::new(Body::from(page)), ArticleResponse::Page)
                .await;
            assert_eq!(
                returned
                    .into_body()
                    .collect()
                    .await
                    .unwrap()
                    .to_bytes()
                    .as_ref(),
                page.as_bytes()
            );
            let counts = receiver.await.unwrap();
            assert_eq!(counts.read_count, Some(97794));
            assert_eq!(counts.comment_count, Some(54));
            assert_eq!(counts.like_count, None);
            assert!(
                ArticleInteractions::parse_html("<script>comment_count = 0;</script>").is_none()
            );
        }

        #[tokio::test]
        async fn missing_article_does_not_start_a_proxy() {
            let store = crate::Store::open_in_memory().unwrap();
            let ca = crate::ca::CaMaterial::generate().unwrap();
            let error = super::super::collect::<()>(
                &store,
                42,
                ca,
                |_| panic!("unexpected proxy setup"),
                || panic!("unexpected ready notification"),
            )
            .await
            .unwrap_err();
            assert_eq!(error.to_string(), "文章不存在");
        }

        #[test]
        fn metrics_updates_keep_previous_fields_when_missing() {
            let store = crate::Store::open_in_memory().unwrap();
            let id = store
                .upsert_article("B", "42", 1, &crate::ArticleFields::default())
                .unwrap();
            store
                .save_article_interactions(
                    id,
                    &ArticleInteractions {
                        read_count: Some(20),
                        comment_count: Some(5),
                        ..Default::default()
                    },
                )
                .unwrap();
            store
                .save_article_interactions(
                    id,
                    &ArticleInteractions {
                        comment_count: Some(0),
                        ..Default::default()
                    },
                )
                .unwrap();
            let rows = store.list_articles(None, 10, 0, false).unwrap();
            assert_eq!(rows[0].interactions.read_count, Some(20));
            assert_eq!(rows[0].interactions.comment_count, Some(0));
            assert_eq!(rows[0].interactions.like_count, None);
        }
    }
}

#[cfg(any(windows, test))]
pub async fn collect<G>(
    store: &crate::Store,
    id: i64,
    ca: crate::ca::CaMaterial,
    install_proxy: impl FnOnce(u16) -> std::result::Result<G, String>,
    ready: impl FnOnce(),
) -> anyhow::Result<ArticleInteractions> {
    use anyhow::{ensure, Context};
    let (article, _) = store.get_article_md(id)?.context("文章不存在")?;
    ensure!(article.is_deleted == 0, "文章已删除");
    let target = capture::ArticleIdentity {
        biz: article.biz,
        mid: article
            .mid
            .filter(|mid| !mid.is_empty())
            .context("文章缺少消息 ID")?,
        idx: article.idx.filter(|idx| *idx > 0).context("文章缺少序号")?,
    };
    let metrics = capture::run(ca, target, install_proxy, ready).await?;
    store.save_article_interactions(id, &metrics)?;
    Ok(metrics)
}
