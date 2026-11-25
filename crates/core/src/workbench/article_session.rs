//! Ephemeral article credentials from the signed-in reader. Never serialized or logged.
#[cfg(windows)]
use anyhow::ensure;
use anyhow::Result;
use std::collections::BTreeMap;
#[cfg(any(windows, test))]
use std::sync::LazyLock;

pub(super) const USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/132.0.0.0 Safari/537.36 MicroMessenger/4.1.15.56";
pub(super) struct Session {
    pub page: url::Url,
    pub fields: BTreeMap<String, String>,
    #[cfg(any(windows, test))]
    timestamp: u64,
}
impl Session {
    #[cfg(any(windows, test))]
    fn parse(raw: &str, biz: &str, mid: &str, idx: i64) -> Option<Self> {
        if raw.len() > 16000 {
            return None;
        }
        let mut page = url::Url::parse(&raw.replace("&amp;", "&")).ok()?;
        if page.host_str() != Some("mp.weixin.qq.com")
            || !matches!(page.path(), "/s" | "/mp/appmsg_comment")
            || !page.username().is_empty()
            || page.password().is_some()
            || page.port().is_some()
        {
            return None;
        }
        let mut fields = BTreeMap::new();
        for (k, v) in page.query_pairs() {
            if fields.get(k.as_ref()).is_some_and(|old| old != v.as_ref()) {
                return None;
            }
            fields.insert(k.into_owned(), v.into_owned());
        }
        let is_api = page.path() == "/mp/appmsg_comment";
        if is_api
            && !matches!(
                fields.get("action").map(String::as_str),
                Some("getcomment" | "getsegment" | "getcommentreply")
            )
        {
            return None;
        }
        if fields.get("__biz")?.as_str() != biz
            || fields
                .get(if is_api { "appmsgid" } else { "mid" })?
                .as_str()
                != mid
            || fields.get("idx")?.parse::<i64>().ok()? != idx
        {
            return None;
        }
        let key = fields.get("key")?;
        let sn = fields.get("sn").map(String::as_str).unwrap_or("");
        if !(64..=2048).contains(&key.len())
            || !key.bytes().all(|b| b.is_ascii_hexdigit())
            || (!is_api && (sn.len() != 32 || !sn.bytes().all(|b| b.is_ascii_hexdigit())))
            || (is_api
                && fields
                    .get("appmsg_token")
                    .is_none_or(|v| v.len() < 8 || v.len() > 4096))
            || fields.get("uin")?.is_empty()
        {
            return None;
        }
        page.set_scheme("https").ok()?;
        page.set_fragment(None);
        let timestamp = fields
            .get(if is_api { "enterid" } else { "clicktime" })
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);
        Some(Self {
            page,
            fields,
            timestamp,
        })
    }
    #[cfg(any(windows, test))]
    fn reusable(raw: &str, biz: &str) -> Option<Self> {
        let url = url::Url::parse(&raw.replace("&amp;", "&")).ok()?;
        if url.path() != "/s" {
            return None;
        }
        let fields: BTreeMap<_, _> = url.query_pairs().into_owned().collect();
        Self::parse(
            raw,
            biz,
            fields.get("mid")?,
            fields.get("idx")?.parse().ok()?,
        )
    }
    #[cfg(any(windows, test))]
    fn retarget(self, link: &str, biz: &str, mid: &str, idx: i64) -> Result<Self> {
        let input = url::Url::parse(link)?;
        let fields: BTreeMap<String, String> = input.query_pairs().into_owned().collect();
        anyhow::ensure!(
            input.host_str() == Some("mp.weixin.qq.com")
                && input.path() == "/s"
                && input.username().is_empty()
                && input.password().is_none()
                && input.port().is_none()
                && matches!(input.scheme(), "http" | "https")
                && fields.get("__biz").map(String::as_str) == Some(biz)
                && fields.get("mid").map(String::as_str) == Some(mid)
                && fields.get("idx").and_then(|v| v.parse::<i64>().ok()) == Some(idx),
            "文章缺少可核对的完整原文链接"
        );
        let sn = fields
            .get("sn")
            .filter(|s| s.len() == 32 && s.bytes().all(|b| b.is_ascii_hexdigit()))
            .ok_or_else(|| anyhow::anyhow!("文章缺少完整原文签名，请重新同步文章链接"))?;
        let mut target = url::Url::parse("https://mp.weixin.qq.com/s")?;
        let mut query = target.query_pairs_mut();
        query
            .append_pair("__biz", biz)
            .append_pair("mid", mid)
            .append_pair("idx", &idx.to_string())
            .append_pair("sn", sn);
        for name in ["chksm", "scene"] {
            if let Some(value) = fields.get(name) {
                query.append_pair(name, value);
            }
        }
        // Reader authentication can be reused; article tokens and comment IDs cannot.
        for name in [
            "key",
            "uin",
            "pass_ticket",
            "devicetype",
            "version",
            "clientversion",
            "wxtoken",
        ] {
            if let Some(value) = self.fields.get(name) {
                query.append_pair(name, value);
            }
        }
        drop(query);
        let fields = target.query_pairs().into_owned().collect();
        Ok(Self {
            page: target,
            fields,
            timestamp: self.timestamp,
        })
    }
    pub fn endpoint(&self, path: &str, biz: &str, token: &str) -> url::Url {
        let mut u = url::Url::parse(&format!("https://mp.weixin.qq.com{path}")).unwrap();
        let mut q = u.query_pairs_mut();
        for k in [
            "key",
            "uin",
            "pass_ticket",
            "devicetype",
            "version",
            "clientversion",
            "wxtoken",
            "scene",
            "sessionid",
            "enterid",
            "user_article_role",
        ] {
            if let Some(v) = self.fields.get(k) {
                q.append_pair(k, v);
            }
        }
        q.append_pair("__biz", biz)
            .append_pair("appmsg_token", token)
            .append_pair("f", "json")
            .append_pair("x5", "0");
        drop(q);
        u
    }
}
#[cfg(any(windows, test))]
fn consider(
    data: &[u8],
    biz: &str,
    mid: &str,
    idx: i64,
    best: &mut Option<Session>,
    api: &mut Option<Session>,
    reusable: &mut Option<Session>,
) {
    static URL: LazyLock<regex::bytes::Regex> = LazyLock::new(|| {
        regex::bytes::Regex::new(
            r#"https?://mp\.weixin\.qq\.com/(?:s|mp/appmsg_comment)\?[^\x00-\x20<>"'\\]+"#,
        )
        .unwrap()
    });
    let mut accept = |bytes: &[u8]| {
        if let Ok(text) = std::str::from_utf8(bytes) {
            if let Some(candidate) = Session::reusable(text, biz) {
                if reusable.as_ref().is_none_or(|old| {
                    candidate.timestamp > old.timestamp
                        || (candidate.timestamp == old.timestamp
                            && candidate.page.as_str().len() > old.page.as_str().len())
                }) {
                    *reusable = Some(candidate);
                }
            }
            if let Some(s) = Session::parse(text, biz, mid, idx) {
                let best = if s.page.path() == "/s" {
                    &mut *best
                } else {
                    &mut *api
                };
                if best.as_ref().is_none_or(|old| {
                    s.timestamp > old.timestamp
                        || (s.timestamp == old.timestamp
                            && s.page.as_str().len() > old.page.as_str().len())
                }) {
                    *best = Some(s);
                }
            }
        }
    };
    for m in URL.find_iter(data) {
        accept(m.as_bytes());
    }
    // Decode only URL-shaped UTF-16 strings, not the entire memory region.
    static WIDE: LazyLock<regex::bytes::Regex> =
        LazyLock::new(|| regex::bytes::Regex::new(r"h\x00t\x00t\x00p\x00s\x00:\x00").unwrap());
    for found in WIDE.find_iter(data) {
        let start = found.start();
        let text: Vec<u8> = data[start..]
            .as_chunks::<2>()
            .0
            .iter()
            .take(16000)
            .take_while(|p| p[1] == 0 && p[0] > 32 && p[0] < 127)
            .map(|p| p[0])
            .collect();
        for m in URL.find_iter(&text) {
            accept(m.as_bytes());
        }
    }
}

#[cfg(any(windows, test))]
fn bind_api(page: &mut Session, api: Session) {
    if page.fields.get("uin") != api.fields.get("uin") {
        return;
    }
    page.timestamp = page.timestamp.max(api.timestamp);
    for name in [
        "key",
        "uin",
        "pass_ticket",
        "devicetype",
        "version",
        "clientversion",
        "wxtoken",
        "scene",
        "sessionid",
        "enterid",
        "user_article_role",
        "appmsg_token",
        "comment_id",
    ] {
        page.fields.remove(name);
        if let Some(value) = api.fields.get(name) {
            page.fields.insert(name.into(), value.clone());
        }
    }
}

#[cfg(not(windows))]
pub(super) fn discover(_biz: &str, _mid: &str, _idx: i64, _link: &str) -> Result<Session> {
    anyhow::bail!("互动补采需要在已登录微信的 Windows 电脑运行")
}

#[cfg(windows)]
pub(super) fn discover(biz: &str, mid: &str, idx: i64, link: &str) -> Result<Session> {
    use std::{
        mem::size_of,
        time::{Duration, Instant},
    };
    use windows::Win32::{
        Foundation::{CloseHandle, HANDLE},
        System::{
            Diagnostics::{
                Debug::ReadProcessMemory,
                ToolHelp::{
                    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
                    TH32CS_SNAPPROCESS,
                },
            },
            Memory::{
                VirtualQueryEx, MEMORY_BASIC_INFORMATION, MEM_COMMIT, MEM_IMAGE,
                PAGE_EXECUTE_READWRITE, PAGE_EXECUTE_WRITECOPY, PAGE_GUARD, PAGE_READWRITE,
                PAGE_WRITECOPY,
            },
            Threading::{OpenProcess, PROCESS_QUERY_INFORMATION, PROCESS_VM_READ},
        },
    };
    struct Handle(HANDLE);
    impl Drop for Handle {
        fn drop(&mut self) {
            unsafe {
                let _ = CloseHandle(self.0);
            }
        }
    }
    ensure!(size_of::<usize>() == 8, "文章会话读取需要 64 位程序");
    let snapshot = Handle(unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0)? });
    let mut entry = PROCESSENTRY32W {
        dwSize: size_of::<PROCESSENTRY32W>() as u32,
        ..Default::default()
    };
    let mut pids = Vec::new();
    if unsafe { Process32FirstW(snapshot.0, &mut entry) }.is_ok() {
        loop {
            let n = entry
                .szExeFile
                .iter()
                .position(|&c| c == 0)
                .unwrap_or(entry.szExeFile.len());
            if String::from_utf16_lossy(&entry.szExeFile[..n])
                .eq_ignore_ascii_case("WeChatAppEx.exe")
            {
                pids.push(entry.th32ProcessID);
            }
            if unsafe { Process32NextW(snapshot.0, &mut entry) }.is_err() {
                break;
            }
        }
    }
    let started = Instant::now();
    let mut best = None;
    let mut api = None;
    let mut reusable = None;
    let mut total = 0usize;
    let mut buffer = vec![0u8; 2 * 1024 * 1024 + 32768];
    for pid in pids {
        let Ok(handle) =
            (unsafe { OpenProcess(PROCESS_VM_READ | PROCESS_QUERY_INFORMATION, false, pid) })
        else {
            continue;
        };
        let handle = Handle(handle);
        let mut address = 0usize;
        let mut process_bytes = 0usize;
        while started.elapsed() < Duration::from_secs(20)
            && total < 2_000_000_000
            && process_bytes < 384_000_000
        {
            let mut mbi = MEMORY_BASIC_INFORMATION::default();
            if unsafe {
                VirtualQueryEx(
                    handle.0,
                    Some(address as *const _),
                    &mut mbi,
                    size_of::<MEMORY_BASIC_INFORMATION>(),
                )
            } == 0
            {
                break;
            }
            let base = mbi.BaseAddress as usize;
            let Some(next) = base.checked_add(mbi.RegionSize).filter(|&n| n > address) else {
                break;
            };
            if mbi.State == MEM_COMMIT
                && mbi.Type != MEM_IMAGE
                && mbi.Protect.0 & PAGE_GUARD.0 == 0
                && [
                    PAGE_READWRITE,
                    PAGE_WRITECOPY,
                    PAGE_EXECUTE_READWRITE,
                    PAGE_EXECUTE_WRITECOPY,
                ]
                .contains(&mbi.Protect)
            {
                for offset in (0..mbi.RegionSize.min(64_000_000)).step_by(2 * 1024 * 1024) {
                    if started.elapsed() >= Duration::from_secs(20)
                        || total >= 2_000_000_000
                        || process_bytes >= 384_000_000
                    {
                        break;
                    }
                    let count = buffer.len().min(mbi.RegionSize - offset);
                    let mut read = 0usize;
                    unsafe {
                        let _ = ReadProcessMemory(
                            handle.0,
                            (base + offset) as *const _,
                            buffer.as_mut_ptr().cast(),
                            count,
                            Some(&mut read),
                        );
                    }
                    let read = read.min(count);
                    total += read;
                    process_bytes += read;
                    consider(
                        &buffer[..read],
                        biz,
                        mid,
                        idx,
                        &mut best,
                        &mut api,
                        &mut reusable,
                    );
                }
            }
            address = next;
        }
    }
    if let (Some(page), Some(api)) = (&mut best, api) {
        bind_api(page, api);
    }
    if let Some(source) = reusable {
        if best
            .as_ref()
            .is_none_or(|target| source.timestamp > target.timestamp)
        {
            return source.retarget(link, biz, mid, idx);
        }
    }
    best.ok_or_else(|| anyhow::anyhow!("没有可复用的微信文章会话"))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn sample() -> String {
        format!(
            "https://mp.weixin.qq.com/s?__biz=B&mid=123&idx=1&sn={}&key={}&uin=U",
            "a".repeat(32),
            "b".repeat(128)
        )
    }
    #[test]
    fn reuse_replaces_article_identity_and_never_copies_article_tokens() {
        let mut source = Session::parse(&sample(), "B", "123", 1).unwrap();
        source
            .fields
            .insert("appmsg_token".into(), "source-token".into());
        source.fields.insert("comment_id".into(), "999999".into());
        let link = format!(
            "https://mp.weixin.qq.com/s?__biz=B&mid=456&idx=2&sn={}",
            "d".repeat(32)
        );
        let target = source.retarget(&link, "B", "456", 2).unwrap();
        assert_eq!(target.fields["mid"], "456");
        assert_eq!(target.fields["sn"], "d".repeat(32));
        assert_eq!(target.fields["key"], "b".repeat(128));
        assert!(!target.fields.contains_key("appmsg_token"));
        assert!(!target.fields.contains_key("comment_id"));
        assert!(Session::parse(&sample(), "B", "123", 1)
            .unwrap()
            .retarget(&link, "OTHER", "456", 2)
            .is_err());
    }

    #[test]
    fn binds_request_token_only_to_the_same_article_and_account() {
        let mut page = Session::parse(&sample(), "B", "123", 1).unwrap();
        let request=format!("https://mp.weixin.qq.com/mp/appmsg_comment?action=getcomment&__biz=B&appmsgid=123&idx=1&key={}&uin=U&appmsg_token=fake-token&comment_id=123456", "c".repeat(128));
        assert!(Session::parse(&request, "B", "321", 1).is_none());
        let foreign =
            Session::parse(&request.replace("uin=U", "uin=OTHER"), "B", "123", 1).unwrap();
        bind_api(&mut page, foreign);
        assert!(!page.fields.contains_key("appmsg_token"));
        bind_api(&mut page, Session::parse(&request, "B", "123", 1).unwrap());
        assert_eq!(page.fields.get("appmsg_token").unwrap(), "fake-token");
        assert_eq!(page.fields.get("key").unwrap(), &"c".repeat(128));
    }
    #[test]
    fn identity_and_credentials_are_bound() {
        let u = sample();
        assert!(Session::parse(&u, "B", "123", 1).is_some());
        assert!(Session::parse(&(u.clone() + "&__biz=B"), "B", "123", 1).is_some());
        assert!(Session::parse(&(u.clone() + "&__biz=OTHER"), "B", "123", 1).is_none());
        assert!(Session::parse(&u, "OTHER", "123", 1).is_none());
        assert!(Session::parse(&(u.clone() + "&mid=321"), "B", "123", 1).is_none());
        assert!(
            Session::parse(&u.replace("mp.weixin.qq.com", "evil.test"), "B", "123", 1).is_none()
        );
    }
    #[test]
    fn scans_ascii_and_utf16_without_joining_strings() {
        let u = sample();
        let mut best = None;
        consider(u.as_bytes(), "B", "123", 1, &mut best, &mut None, &mut None);
        assert!(best.is_some());
        let wide: Vec<u8> = u.encode_utf16().flat_map(u16::to_le_bytes).collect();
        let mut best = None;
        consider(&wide, "B", "123", 1, &mut best, &mut None, &mut None);
        assert!(best.is_some());
    }
}
