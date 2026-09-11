//! 抓凭证纯函数（对齐 Python 原型的“纯函数区”）。
//!
//! 只吃字符串，返回结构化数据，可脱离代理单测。MITM 钩子（见 `capture` 模块）
//! 只负责取值、调这里的纯函数、写库。移植了 Python 原型的断言
//! （见本文件 `#[cfg(test)]`）。

use std::collections::HashMap;
use std::sync::LazyLock;

use regex::Regex;

/// 只关心这个 host（微信内置浏览器 WebView 走系统代理、可解密的 HTTP 接口）。
pub const TARGET_HOST: &str = "mp.weixin.qq.com";

/// 凭证里除 __biz 外要提取的字段名（URL query / POST body / Cookie 通用）。
pub const CRED_KEYS: [&str; 6] = ["uin", "key", "pass_ticket", "wxtoken", "x5", "appmsg_token"];

/// 判定“这批凭证是否含真正会话材料”的关键字段。
pub const SESSION_KEYS: [&str; 4] = ["key", "uin", "pass_ticket", "appmsg_token"];

/// 被证书固定 / 走 mmtls 的微信长连接域名后缀，主动 TLS 直通。
/// 绝不能包含 mp.weixin.qq.com —— 那是我们要拦截解密的目标。
const PINNED_SUFFIXES: [&str; 4] = [
    "long.weixin.qq.com",  // long / szlong / hklong ...
    "short.weixin.qq.com", // short / extshort / minorshort ...
    "dns.weixin.qq.com",
    "wxancra.weixin.qq.com",
];

/// 抽取出的凭证（对齐 Python 返回的 dict，只含“非空”字段用 `Some` 表示）。
///
/// `biz` 单列（对应 Python 里 `out["biz"]`）；其余对应 `CRED_KEYS`。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Creds {
    pub biz: Option<String>,
    pub uin: Option<String>,
    pub key: Option<String>,
    pub pass_ticket: Option<String>,
    pub wxtoken: Option<String>,
    pub x5: Option<String>,
    pub appmsg_token: Option<String>,
}

impl Creds {
    /// 取某个 `CRED_KEYS` 字段（供泛化访问）。
    fn get(&self, k: &str) -> Option<&str> {
        match k {
            "uin" => self.uin.as_deref(),
            "key" => self.key.as_deref(),
            "pass_ticket" => self.pass_ticket.as_deref(),
            "wxtoken" => self.wxtoken.as_deref(),
            "x5" => self.x5.as_deref(),
            "appmsg_token" => self.appmsg_token.as_deref(),
            _ => None,
        }
    }
}

/// 解析出的 `/s?` 文章骨架（对齐 parse_s_url 返回）。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SUrl {
    pub biz: String,
    pub mid: String,
    pub idx: i64,
    pub sn: Option<String>,
    pub chksm: Option<String>,
    pub content_url: String,
}

/// 规整 `__biz`：仅做去首尾空白（百分号解码由查询串解析负责）。
pub fn normalize_biz(value: Option<&str>) -> String {
    value.map(|v| v.trim().to_string()).unwrap_or_default()
}

/// 取 URL 的 query 段（不含 `?`，截断 `#fragment`）。对齐 Python urlsplit(url).query。
fn url_query(url: &str) -> &str {
    let after = match url.split_once('?') {
        Some((_, q)) => q,
        None => return "",
    };
    match after.split_once('#') {
        Some((q, _)) => q,
        None => after,
    }
}

/// 把 `a=1&b=2` 形式的串解析进 `merged`（就地）。
///
/// 规则（对齐 Python `_merge_pairs`）：后写覆盖先写；但**空值不覆盖已有非空值**，
/// 防止占位请求里的 `key=` 把之前抓到的真实 key 清掉。百分号解码 + `+`→空格
/// 由 `form_urlencoded` 负责（等价 parse_qsl(keep_blank_values=True)）。
fn merge_pairs(merged: &mut HashMap<String, String>, text: &str) {
    if text.is_empty() {
        return;
    }
    for (k, v) in form_urlencoded::parse(text.as_bytes()) {
        let k = k.into_owned();
        let v = v.into_owned();
        if v.is_empty() && merged.get(&k).is_some_and(|e| !e.is_empty()) {
            continue;
        }
        merged.insert(k, v);
    }
}

/// 解析 Cookie 头 `a=1; b=2` 成 map（对齐 Python `_parse_cookie`）。
fn parse_cookie(cookie: Option<&str>) -> HashMap<String, String> {
    let mut out = HashMap::new();
    let Some(cookie) = cookie else {
        return out;
    };
    for part in cookie.split(';') {
        let part = part.trim();
        if part.is_empty() || !part.contains('=') {
            continue;
        }
        if let Some((k, v)) = part.split_once('=') {
            out.insert(k.trim().to_string(), v.trim().to_string());
        }
    }
    out
}

/// 从 URL query + POST body + Cookie 头合并提取凭证字段。
///
/// 合并优先级：Cookie < URL query < POST body（越靠后越权威）。
/// 只含非空字段；命中的 `__biz` 放到 `Creds::biz`。
pub fn extract_credentials(url: &str, body: Option<&str>, cookie: Option<&str>) -> Creds {
    let mut merged: HashMap<String, String> = HashMap::new();
    for (name, val) in parse_cookie(cookie) {
        if !val.is_empty() {
            merged.insert(name, val);
        }
    }
    merge_pairs(&mut merged, url_query(url));
    if let Some(body) = body {
        merge_pairs(&mut merged, body);
    }

    let mut out = Creds::default();
    let biz = normalize_biz(merged.get("__biz").map(String::as_str));
    if !biz.is_empty() {
        out.biz = Some(biz);
    }
    for k in CRED_KEYS {
        if let Some(v) = merged.get(k) {
            if !v.is_empty() {
                match k {
                    "uin" => out.uin = Some(v.clone()),
                    "key" => out.key = Some(v.clone()),
                    "pass_ticket" => out.pass_ticket = Some(v.clone()),
                    "wxtoken" => out.wxtoken = Some(v.clone()),
                    "x5" => out.x5 = Some(v.clone()),
                    "appmsg_token" => out.appmsg_token = Some(v.clone()),
                    _ => {}
                }
            }
        }
    }
    out
}

/// 凭证里是否含真正的会话材料（key/uin/pass_ticket/appmsg_token 任一非空）。
pub fn has_session(creds: &Creds) -> bool {
    SESSION_KEYS
        .iter()
        .any(|k| creds.get(k).is_some_and(|v| !v.is_empty()))
}

/// 构造干净、可回放的文章 URL，剔除 scene/sessionid 等易变参数。
fn canonical_s_url(
    biz: &str,
    mid: &str,
    idx: i64,
    sn: Option<&str>,
    chksm: Option<&str>,
) -> String {
    let mut parts = vec![
        format!("__biz={biz}"),
        format!("mid={mid}"),
        format!("idx={idx}"),
    ];
    if let Some(sn) = sn {
        parts.push(format!("sn={sn}"));
    }
    if let Some(chksm) = chksm {
        parts.push(format!("chksm={chksm}"));
    }
    format!("https://mp.weixin.qq.com/s?{}", parts.join("&"))
}

/// 解析 `/s?__biz=..&mid=..&idx=..&sn=..` 文章页 URL。
///
/// 命中返回 `Some(SUrl)`；不是该形式或缺 `__biz/mid` 时返回 `None`。idx 缺省按 1。
/// 只认 query 形式的 `/s?`；短链 `/s/<hash>` 无参数，交给响应 HTML 解析。
pub fn parse_s_url(url: &str) -> Option<SUrl> {
    // 用 url crate 稳健取 path；解析失败视为不命中。
    let parsed = url::Url::parse(url).ok()?;
    if parsed.path() != "/s" {
        return None;
    }
    let query = url_query(url);
    if query.is_empty() {
        return None;
    }
    let mut q: HashMap<String, String> = HashMap::new();
    merge_pairs(&mut q, query);
    let biz = normalize_biz(q.get("__biz").map(String::as_str));
    let mid = q.get("mid").cloned().unwrap_or_default();
    if biz.is_empty() || mid.is_empty() {
        return None;
    }
    let idx = q
        .get("idx")
        .and_then(|s| if s.is_empty() { None } else { Some(s.as_str()) })
        .and_then(|s| s.parse::<i64>().ok())
        .unwrap_or(1);
    let sn = q.get("sn").filter(|s| !s.is_empty()).cloned();
    let chksm = q.get("chksm").filter(|s| !s.is_empty()).cloned();
    let content_url = canonical_s_url(&biz, &mid, idx, sn.as_deref(), chksm.as_deref());
    Some(SUrl {
        biz,
        mid,
        idx,
        sn,
        chksm,
        content_url,
    })
}

/// 「批量添加」输入的一行 → 可入队的公众号文章链接。
///
/// 只接受 `mp.weixin.qq.com` 上的文章页：长链 `/s?__biz=…&mid=…`（规范化为干净的可回放 URL，剔除
/// `scene` / `sessionid` 等易变参数）与短链 `/s/<id>`（原样保留，去掉 fragment）。其余（空行、别的域名、
/// 公众号主页 `profile_ext`、不带 `__biz`/`mid` 的 `/s?`）返回 `Err(原因)`。
pub fn normalize_article_link(raw: &str) -> std::result::Result<String, &'static str> {
    let t = raw.trim();
    if t.is_empty() {
        return Err("空行");
    }
    let parsed = url::Url::parse(t).map_err(|_| "不是合法的 URL")?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err("只支持 http / https 链接");
    }
    if parsed.host_str().map(str::to_ascii_lowercase).as_deref() != Some(TARGET_HOST) {
        return Err("不是 mp.weixin.qq.com 上的文章链接");
    }
    let path = parsed.path();
    if path == "/s" {
        return parse_s_url(t)
            .map(|p| p.content_url)
            .ok_or("长链缺少 __biz / mid 参数");
    }
    if let Some(id) = path.strip_prefix("/s/") {
        if id.is_empty() || id.contains('/') {
            return Err("短链缺少文章 id");
        }
        let mut u = parsed.clone();
        u.set_fragment(None);
        return Ok(u.to_string());
    }
    Err("不是文章页链接（只认 /s?… 长链或 /s/… 短链）")
}

/// 是否 `mp.weixin.qq.com/s/<id>` 形式的文章短链（不带 `__biz` 参数，打开时由微信 302 到长链）。
pub fn is_short_article_link(url: &str) -> bool {
    let Ok(parsed) = url::Url::parse(url) else {
        return false;
    };
    parsed.host_str().map(str::to_ascii_lowercase).as_deref() == Some(TARGET_HOST)
        && parsed
            .path()
            .strip_prefix("/s/")
            .is_some_and(|id| !id.is_empty() && !id.contains('/'))
}

/// 「批量添加公众号」输入的一行 → 可用的文章**短链**。
///
/// 只接受短链（文章右上角「复制链接」得到的 `https://mp.weixin.qq.com/s/<id>`）：短链匿名直连稳定回正文页，
/// 长链没有 `chksm` 参数时匿名访问必回验证页、拿不到公众号名称，所以一律拒绝并提示改用短链。
pub fn require_short_article_link(raw: &str) -> std::result::Result<String, &'static str> {
    let u = normalize_article_link(raw)?;
    if is_short_article_link(&u) {
        Ok(u)
    } else {
        Err("只接受短链：请在文章页右上角「复制链接」，粘贴 https://mp.weixin.qq.com/s/… 形式的链接")
    }
}

/// 该 host 是否属于被证书固定 / 走 mmtls 的微信主协议域名，需要 TLS 直通。
/// 显式排除 mp.weixin.qq.com（那是我们要解密的目标，绝不直通）。
pub fn is_wechat_pinned_host(host: Option<&str>) -> bool {
    let Some(host) = host else {
        return false;
    };
    if host.is_empty() {
        return false;
    }
    let h = host.to_ascii_lowercase();
    if h == TARGET_HOST {
        return false;
    }
    if h.starts_with("mmtls.") {
        return true;
    }
    for suf in PINNED_SUFFIXES {
        if h == suf || h.ends_with(&format!(".{suf}")) || h.ends_with(suf) {
            return true;
        }
    }
    false
}

// ---------------------------------------------------------------------------
// HTML 解析（对齐 Python selectolax 版本；用 scraper crate 做 DOM，regex 兜底 JS 变量）
// ---------------------------------------------------------------------------

/// 从公众号文章 HTML 解析**公众号名称**：优先 `#js_name`，再退到 `var nickname`（裸引号或
/// `htmlDecode("…")` 包裹两种写法）。
pub fn parse_account_name_from_html(html: Option<&str>) -> Option<String> {
    let html = html?;
    if html.is_empty() {
        return None;
    }
    let doc = scraper::Html::parse_document(html);
    if let Ok(sel) = scraper::Selector::parse("#js_name") {
        if let Some(node) = doc.select(&sel).next() {
            let t = node.text().collect::<String>();
            let t = t.trim();
            if !t.is_empty() {
                return Some(t.to_string());
            }
        }
    }
    static NICK_RE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r#"var\s+nickname\s*=\s*(?:htmlDecode\()?["'](.*?)["']"#).unwrap()
    });
    if let Some(cap) = NICK_RE.captures(html) {
        let v = cap.get(1).map(|m| m.as_str().trim()).unwrap_or("");
        if !v.is_empty() {
            return Some(v.to_string());
        }
    }
    None
}

/// 从公众号文章 HTML 解析**公众号头像 URL**：`var round_head_img`（退到 `ori_head_img_url` /
/// `hd_head_img_url`）。微信页里 URL 常带转义斜杠 `http:\/\/…`，这里还原成正常斜杠。
pub fn parse_account_avatar_from_html(html: Option<&str>) -> Option<String> {
    let html = html?;
    if html.is_empty() {
        return None;
    }
    static AVATAR_RE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(
            r#"var\s+(?:round_head_img|ori_head_img_url|hd_head_img_url)\s*=\s*["']([^"']+)["']"#,
        )
        .unwrap()
    });
    for cap in AVATAR_RE.captures_iter(html) {
        let v = cap.get(1).map(|m| m.as_str().trim()).unwrap_or("");
        let v = v.replace("\\/", "/");
        if v.starts_with("http") {
            return Some(v);
        }
    }
    None
}

/// 从文章 HTML 内嵌 JS 里提取 `biz/mid/idx/sn`（短链 /s/<hash> 兜底用）。
///
/// 认两种写法：
/// - 变量声明 `var biz = "" || "MzA5...";`（旧版）
/// - 对象属性 `reportOpt: { biz: "Mzg5..." || "", ... }`（新版文章页常见）
///
/// 取 `var NAME =` / `NAME:` 到分号/逗号/换行前的整段表达式，再挑最后一个非空取值
/// （`"" || "真值"` 取真值，`"真值" || ""` 也取真值）。
pub fn parse_article_meta_from_html(html: Option<&str>) -> HashMap<String, String> {
    let mut out: HashMap<String, String> = HashMap::new();
    let Some(html) = html else {
        return out;
    };
    if html.is_empty() {
        return out;
    }
    static QUOTED_RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r#""([^"]*)"|'([^']*)'"#).unwrap());
    static BARE_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"([0-9A-Za-z=]+)").unwrap());

    for name in ["biz", "mid", "idx", "sn"] {
        // 认两种写法：变量声明 `var biz = "" || "Mz==";` 与对象属性
        // `reportOpt: { biz: "Mz==" || "", ... }`（新版文章页常见，值以逗号结尾）。
        let var_re =
            Regex::new(&format!(r"(?:var\s+{name}\s*=|\b{name}\s*:)\s*([^;,\n]+)")).unwrap();
        let Some(cap) = var_re.captures(html) else {
            continue;
        };
        let expr = cap.get(1).map(|m| m.as_str()).unwrap_or("");
        // 取表达式里所有引号字符串，选最后一个非空。
        let quoted: Vec<String> = QUOTED_RE
            .captures_iter(expr)
            .filter_map(|c| {
                let a = c.get(1).map(|m| m.as_str());
                let b = c.get(2).map(|m| m.as_str());
                let v = a.or(b).unwrap_or("");
                if v.is_empty() {
                    None
                } else {
                    Some(v.to_string())
                }
            })
            .collect();
        let val = if let Some(last) = quoted.last() {
            last.trim().to_string()
        } else if let Some(m) = BARE_RE.captures(expr).and_then(|c| c.get(1)) {
            m.as_str().to_string()
        } else {
            String::new()
        };
        if !val.is_empty() {
            out.insert(name.to_string(), val);
        }
    }
    if let Some(biz) = out.get("biz").cloned() {
        out.insert("biz".to_string(), normalize_biz(Some(&biz)));
    }
    out
}

/// 用文章 HTML 提取到的 meta（biz/mid/idx/sn）拼出规范**签名长链**
/// `/s?__biz=..&mid=..&idx=..&sn=..`；缺 biz 或 mid 时返回 `None`。
///
/// 短链 `/s/<hash>` 的请求 URL 无 `__biz`，抓不到凭证；把 HTML 里的 biz/mid/idx/sn
/// 拼回长链后再接力打开，长链页会触发带 key 的凭证请求，从而能抓到凭证、采集该号。
pub fn long_url_from_meta(meta: &HashMap<String, String>) -> Option<String> {
    let biz = meta.get("biz").filter(|s| !s.is_empty())?;
    let mid = meta.get("mid").filter(|s| !s.is_empty())?;
    let idx = meta
        .get("idx")
        .and_then(|s| {
            if s.is_empty() {
                None
            } else {
                s.parse::<i64>().ok()
            }
        })
        .unwrap_or(1);
    let sn = meta.get("sn").filter(|s| !s.is_empty()).map(String::as_str);
    Some(canonical_s_url(biz, mid, idx, sn, None))
}

#[cfg(test)]
mod tests {
    use super::*;

    // 测试夹具：结构与真实请求一致，但 biz / key / sn 全是假值。
    const VALID_JSMONITOR: &str = "https://mp.weixin.qq.com/mp/jsmonitor?uin=MTIzNDU2Nzg5&key=0123456789abcdef0123456789abcdef";
    const S_URL: &str = "https://mp.weixin.qq.com/s?__biz=VEVTVEJJWjAwMQ==&mid=2650399036&idx=2&sn=00112233445566778899aabbccddeeff&chksm=0011223344556677&scene=126&sessionid=1788228225&subscene=7";
    const S_URL_ENCODED: &str = "https://mp.weixin.qq.com/s?__biz=VEVTVEJJWjAwMQ%3D%3D&mid=2650399036&idx=1&sn=deadbeefdeadbeef";
    const PLACEHOLDER: &str = "https://mp.weixin.qq.com/mp/getbizbanner?__biz=VEVTVEJJWjAwMQ==&uin=&key=&pass_ticket=&wxtoken=777&x5=0&appmsg_token=&f=json";

    #[test]
    fn test_normalize_article_link_accepts_long_and_short_only() {
        // 长链：规范化、剔除易变参数
        let long = normalize_article_link(
            "  https://mp.weixin.qq.com/s?__biz=VEVTVEJJWjAwMQ==&mid=1&idx=2&sn=ab&scene=126&sessionid=9 ",
        )
        .unwrap();
        assert_eq!(
            long,
            "https://mp.weixin.qq.com/s?__biz=VEVTVEJJWjAwMQ==&mid=1&idx=2&sn=ab"
        );
        // 短链：原样保留（去 fragment）
        assert_eq!(
            normalize_article_link("http://mp.weixin.qq.com/s/AbC_12-xyz#rd").unwrap(),
            "http://mp.weixin.qq.com/s/AbC_12-xyz"
        );
        // 拒绝：空行 / 别的域名 / 主页 / 缺参数 / 非 URL
        assert_eq!(normalize_article_link("   "), Err("空行"));
        assert!(normalize_article_link("https://example.com/s?__biz=A&mid=1").is_err());
        assert!(normalize_article_link(
            "https://mp.weixin.qq.com/mp/profile_ext?action=home&__biz=A"
        )
        .is_err());
        assert!(normalize_article_link("https://mp.weixin.qq.com/s?scene=1").is_err());
        assert!(normalize_article_link("https://mp.weixin.qq.com/s/").is_err());
        assert!(normalize_article_link("not a url").is_err());
    }

    #[test]
    fn test_normalize_biz_keeps_double_equals() {
        assert_eq!(normalize_biz(Some("VEVTVEJJWjAwMQ==")), "VEVTVEJJWjAwMQ==");
        assert_eq!(
            normalize_biz(Some("  VEVTVEJJWjAwMQ==  ")),
            "VEVTVEJJWjAwMQ=="
        );
        assert_eq!(normalize_biz(Some("")), "");
        assert_eq!(normalize_biz(None), "");
    }

    #[test]
    fn test_extract_credentials_from_url_query() {
        let creds = extract_credentials(VALID_JSMONITOR, None, None);
        assert_eq!(creds.uin.as_deref(), Some("MTIzNDU2Nzg5"));
        assert_eq!(
            creds.key.as_deref(),
            Some("0123456789abcdef0123456789abcdef")
        );
        assert!(creds.biz.is_none()); // 该请求 URL 无 __biz
    }

    #[test]
    fn test_extract_credentials_biz_and_encoding() {
        let c1 = extract_credentials(
            "https://mp.weixin.qq.com/mp/x?__biz=VEVTVEJJWjAwMQ==&key=K",
            None,
            None,
        );
        assert_eq!(c1.biz.as_deref(), Some("VEVTVEJJWjAwMQ=="));
        // 百分号编码 %3D%3D 由查询串解析解码回 ==
        let c2 = extract_credentials(
            "https://mp.weixin.qq.com/mp/x?__biz=VEVTVEJJWjAwMQ%3D%3D&key=K",
            None,
            None,
        );
        assert_eq!(c2.biz.as_deref(), Some("VEVTVEJJWjAwMQ=="));
    }

    #[test]
    fn test_extract_credentials_skips_empty_and_placeholder() {
        let creds = extract_credentials(PLACEHOLDER, None, None);
        assert_eq!(creds.biz.as_deref(), Some("VEVTVEJJWjAwMQ=="));
        assert!(creds.uin.is_none());
        assert!(creds.key.is_none());
        assert!(creds.pass_ticket.is_none());
        assert!(creds.appmsg_token.is_none());
        assert_eq!(creds.wxtoken.as_deref(), Some("777"));
        assert!(!has_session(&creds));
    }

    #[test]
    fn test_extract_credentials_body_and_cookie_merge() {
        let url = "https://mp.weixin.qq.com/mp/getappmsgext?__biz=BIZ==&x5=0";
        let body = "mid=123&sn=abc&appmsg_token=TOKEN123&key=KEYFROMBODY";
        let cookie = "pass_ticket=PT_FROM_COOKIE; uin=UIN_COOKIE; other=x";
        let creds = extract_credentials(url, Some(body), Some(cookie));
        assert_eq!(creds.biz.as_deref(), Some("BIZ=="));
        assert_eq!(creds.appmsg_token.as_deref(), Some("TOKEN123"));
        assert_eq!(creds.key.as_deref(), Some("KEYFROMBODY")); // 来自 body
        assert_eq!(creds.pass_ticket.as_deref(), Some("PT_FROM_COOKIE")); // 来自 cookie
        assert_eq!(creds.uin.as_deref(), Some("UIN_COOKIE"));
        assert_eq!(creds.x5.as_deref(), Some("0"));
        assert!(has_session(&creds));
    }

    #[test]
    fn test_extract_credentials_empty_does_not_override_nonempty() {
        let url = "https://mp.weixin.qq.com/mp/x?__biz=B==&key=REALKEY";
        let body = "key=&mid=1";
        let creds = extract_credentials(url, Some(body), None);
        assert_eq!(creds.key.as_deref(), Some("REALKEY"));
    }

    #[test]
    fn test_has_session() {
        assert!(has_session(&Creds {
            key: Some("k".into()),
            ..Default::default()
        }));
        assert!(has_session(&Creds {
            uin: Some("u".into()),
            ..Default::default()
        }));
        assert!(has_session(&Creds {
            pass_ticket: Some("p".into()),
            ..Default::default()
        }));
        assert!(has_session(&Creds {
            appmsg_token: Some("a".into()),
            ..Default::default()
        }));
        assert!(!has_session(&Creds {
            wxtoken: Some("777".into()),
            x5: Some("0".into()),
            ..Default::default()
        }));
        assert!(!has_session(&Creds::default()));
    }

    #[test]
    fn test_parse_s_url_basic() {
        let art = parse_s_url(S_URL).expect("should parse");
        assert_eq!(art.biz, "VEVTVEJJWjAwMQ==");
        assert_eq!(art.mid, "2650399036");
        assert_eq!(art.idx, 2);
        assert_eq!(art.sn.as_deref(), Some("00112233445566778899aabbccddeeff"));
        assert_eq!(art.chksm.as_deref(), Some("0011223344556677"));
        assert_eq!(
            art.content_url,
            "https://mp.weixin.qq.com/s?__biz=VEVTVEJJWjAwMQ==&mid=2650399036&idx=2&sn=00112233445566778899aabbccddeeff&chksm=0011223344556677"
        );
    }

    #[test]
    fn test_parse_s_url_encoded_biz_and_default_idx() {
        let art = parse_s_url(S_URL_ENCODED).expect("should parse");
        assert_eq!(art.biz, "VEVTVEJJWjAwMQ==");
        assert_eq!(art.idx, 1);
    }

    #[test]
    fn test_parse_s_url_rejects_non_article() {
        assert!(parse_s_url("https://mp.weixin.qq.com/mp/jsmonitor?a=1").is_none());
        // 短链 /s/<hash> 无 query
        assert!(parse_s_url("https://mp.weixin.qq.com/s/cOQPRdgT5_DYyd1XOUabrQ").is_none());
        assert!(is_short_article_link(
            "https://mp.weixin.qq.com/s/cOQPRdgT5_DYyd1XOUabrQ"
        ));
        assert!(!is_short_article_link(
            "https://mp.weixin.qq.com/s?__biz=A==&mid=1&idx=1"
        ));
        assert!(!is_short_article_link("https://example.com/s/abc"));
        assert_eq!(
            require_short_article_link("https://mp.weixin.qq.com/s/cOQPRdgT5_DYyd1XOUabrQ#rd")
                .unwrap(),
            "https://mp.weixin.qq.com/s/cOQPRdgT5_DYyd1XOUabrQ"
        );
        assert!(require_short_article_link(
            "https://mp.weixin.qq.com/s?__biz=A==&mid=1&idx=1&sn=x"
        )
        .unwrap_err()
        .contains("只接受短链"));
        assert!(require_short_article_link("https://example.com/x").is_err());
        // 缺 mid
        assert!(parse_s_url("https://mp.weixin.qq.com/s?__biz=B==").is_none());
    }

    #[test]
    fn test_parse_article_meta_from_js() {
        let html = "var biz = \"\" || \"VEVTVEJJWjAwMQ==\";\nvar mid = \"\" || \"2650399036\";\nvar idx = \"\" || \"2\";\nvar sn = \"\" || \"00112233445566778899aabbccddeeff\";\n";
        let meta = parse_article_meta_from_html(Some(html));
        assert_eq!(
            meta.get("biz").map(String::as_str),
            Some("VEVTVEJJWjAwMQ==")
        );
        assert_eq!(meta.get("mid").map(String::as_str), Some("2650399036"));
        assert_eq!(meta.get("idx").map(String::as_str), Some("2"));
        assert_eq!(
            meta.get("sn").map(String::as_str),
            Some("00112233445566778899aabbccddeeff")
        );
    }

    #[test]
    fn test_parse_article_meta_from_reportopt() {
        // 新版文章页的对象属性写法（真实短链 /s/AS9Hu9lDqIZWDuan2lXzNw 的原文片段）。
        let html = r#"
      reportOpt: {
        uin: '',
        biz: "VEVTVEJJWjAwMg==" || "",
        mid: "2247534418" || "" || "",
        idx: "2" || "" || "",
        sn: "ffeeddccbbaa99887766554433221100" || "" || "",
      },
"#;
        let meta = parse_article_meta_from_html(Some(html));
        assert_eq!(
            meta.get("biz").map(String::as_str),
            Some("VEVTVEJJWjAwMg==")
        );
        assert_eq!(meta.get("mid").map(String::as_str), Some("2247534418"));
        assert_eq!(meta.get("idx").map(String::as_str), Some("2"));
        assert_eq!(
            meta.get("sn").map(String::as_str),
            Some("ffeeddccbbaa99887766554433221100")
        );
    }

    #[test]
    fn test_long_url_from_meta() {
        // 短链解析出的 meta 拼回规范签名长链。
        let mut meta = HashMap::new();
        meta.insert("biz".to_string(), "VEVTVEJJWjAwMg==".to_string());
        meta.insert("mid".to_string(), "2247534418".to_string());
        meta.insert("idx".to_string(), "2".to_string());
        meta.insert(
            "sn".to_string(),
            "ffeeddccbbaa99887766554433221100".to_string(),
        );
        let url = long_url_from_meta(&meta).unwrap();
        assert_eq!(
            url,
            "https://mp.weixin.qq.com/s?__biz=VEVTVEJJWjAwMg==&mid=2247534418&idx=2&sn=ffeeddccbbaa99887766554433221100"
        );
        // 反解回来 biz/mid/sn 一致，证明是合法长链。
        let s = parse_s_url(&url).unwrap();
        assert_eq!(s.biz, "VEVTVEJJWjAwMg==");
        assert_eq!(s.mid, "2247534418");
        assert_eq!(s.sn.as_deref(), Some("ffeeddccbbaa99887766554433221100"));
        // 缺 biz/mid → None
        assert!(long_url_from_meta(&HashMap::new()).is_none());
    }

    #[test]
    fn test_parse_account_name_from_var_nickname() {
        let html = r#"<html><body><script>var nickname = "机器之心";</script></body></html>"#;
        assert_eq!(
            parse_account_name_from_html(Some(html)).as_deref(),
            Some("机器之心")
        );
    }

    #[test]
    fn test_parse_account_name_prefers_js_name_element() {
        let html = r#"<strong id="js_name">量子位</strong><script>var nickname = "别的";</script>"#;
        assert_eq!(
            parse_account_name_from_html(Some(html)).as_deref(),
            Some("量子位")
        );
    }

    #[test]
    fn test_parse_account_avatar_from_round_head_img() {
        // 含转义斜杠，应还原成正常斜杠。
        let html = r#"<script>var round_head_img = "http:\/\/mmbiz.qpic.cn\/mmbiz_png/abc/0?wx_fmt=png";</script>"#;
        assert_eq!(
            parse_account_avatar_from_html(Some(html)).as_deref(),
            Some("http://mmbiz.qpic.cn/mmbiz_png/abc/0?wx_fmt=png"),
        );
    }

    #[test]
    fn test_parse_account_avatar_none_when_missing() {
        assert_eq!(parse_account_avatar_from_html(Some("<html></html>")), None);
        assert_eq!(parse_account_avatar_from_html(None), None);
    }

    #[test]
    fn test_is_wechat_pinned_host() {
        let cases: [(Option<&str>, bool); 12] = [
            (Some("mp.weixin.qq.com"), false), // 目标 host，绝不直通
            (Some("long.weixin.qq.com"), true),
            (Some("szlong.weixin.qq.com"), true),
            (Some("hklong.weixin.qq.com"), true),
            (Some("short.weixin.qq.com"), true),
            (Some("extshort.weixin.qq.com"), true),
            (Some("dns.weixin.qq.com"), true),
            (Some("mmtls.weixin.qq.com"), true),
            (Some("api.weixin.qq.com"), false),
            (Some("example.com"), false),
            (None, false),
            (Some(""), false),
        ];
        for (host, expected) in cases {
            assert_eq!(is_wechat_pinned_host(host), expected, "host={host:?}");
        }
    }
}
