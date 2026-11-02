//! Windows 只读扫描。主进程每轮都扫，其他进程定期兜底；所有句柄以 RAII 关闭。
use super::*;
use ::windows::Win32::{
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
use std::{
    collections::{HashMap, HashSet},
    mem::size_of,
    time::Instant,
};

struct Handle(HANDLE);
impl Drop for Handle {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}
#[derive(Default)]
pub struct WindowsScanner {
    round: u64,
    hot: HashMap<u32, Instant>,
    seen: HashSet<String>,
}
impl Scanner for WindowsScanner {
    fn scan(&mut self, cancel: &AtomicBool) -> Result<Scan> {
        ensure!(size_of::<usize>() == 8, "推送监测需要 64 位程序");
        let snapshot = Handle(unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0)? });
        let mut entry = PROCESSENTRY32W {
            dwSize: size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };
        let mut processes = Vec::new();
        if unsafe { Process32FirstW(snapshot.0, &mut entry) }.is_ok() {
            loop {
                let n = entry
                    .szExeFile
                    .iter()
                    .position(|&c| c == 0)
                    .unwrap_or(entry.szExeFile.len());
                let name = String::from_utf16_lossy(&entry.szExeFile[..n]).to_ascii_lowercase();
                if ["weixin.exe", "wechat.exe", "wechatappex.exe"].contains(&name.as_str()) {
                    processes.push((entry.th32ProcessID, name));
                }
                if unsafe { Process32NextW(snapshot.0, &mut entry) }.is_err() {
                    break;
                }
            }
        }
        let full = self.round.is_multiple_of(30);
        self.round += 1;
        let live: HashSet<u32> = processes.iter().map(|p| p.0).collect();
        self.hot
            .retain(|pid, at| live.contains(pid) && at.elapsed() < Duration::from_secs(86400));
        let mut scan = Scan {
            processes: processes.len(),
            ..Default::default()
        };
        let mut links = BTreeMap::<String, Link>::new();
        let mut buffer = vec![0u8; 8 * 1024 * 1024 + 8192];
        // 不使用 TTL 筛掉主进程；即使长期无推送，仍能在下一轮观察到新 URL。
        for (pid, name) in processes {
            if cancel.load(Ordering::Relaxed) {
                break;
            }
            if !full && name == "wechatappex.exe" && !self.hot.contains_key(&pid) {
                continue;
            }
            let handle = match unsafe {
                OpenProcess(PROCESS_VM_READ | PROCESS_QUERY_INFORMATION, false, pid)
            } {
                Ok(h) => Handle(h),
                Err(_) => {
                    scan.inaccessible += 1;
                    continue;
                }
            };
            let mut address = 0usize;
            while !cancel.load(Ordering::Relaxed) {
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
                let writable = [
                    PAGE_READWRITE,
                    PAGE_WRITECOPY,
                    PAGE_EXECUTE_READWRITE,
                    PAGE_EXECUTE_WRITECOPY,
                ]
                .contains(&mbi.Protect);
                if mbi.State == MEM_COMMIT
                    && mbi.Type != MEM_IMAGE
                    && mbi.Protect.0 & PAGE_GUARD.0 == 0
                    && writable
                {
                    let mut offset = 0usize;
                    while offset < mbi.RegionSize && !cancel.load(Ordering::Relaxed) {
                        let count = buffer.len().min(mbi.RegionSize - offset);
                        let mut read = 0usize;
                        // 部分页面失效时仍可使用已读取的前缀；不把未初始化区域交给解析器。
                        unsafe {
                            let _ = ReadProcessMemory(
                                handle.0,
                                (base + offset) as *const _,
                                buffer.as_mut_ptr().cast(),
                                count,
                                Some(&mut read),
                            );
                        }
                        read = read.min(count);
                        scan.read_bytes += read as u64;
                        for link in discover(&buffer[..read]) {
                            if self.seen.insert(link.key.clone()) {
                                self.hot.insert(pid, Instant::now());
                            }
                            let replace = links.get(&link.key).is_none_or(|old| {
                                !old.url.contains("chksm=") && link.url.contains("chksm=")
                            });
                            if replace && links.len() < MAX_CANDIDATES {
                                links.insert(link.key.clone(), link);
                            }
                        }
                        offset = offset.saturating_add(8 * 1024 * 1024);
                    }
                }
                address = next;
            }
        }
        if self.seen.len() > 100_000 {
            self.seen.clear();
        }
        scan.links = links.into_values().collect();
        Ok(scan)
    }
}
