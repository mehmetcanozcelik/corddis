//! Windows'a özel küçük yardımcılar.

/// Arayüz açılamadığında (konsol yok) hatayı göstermek için basit mesaj kutusu.
#[cfg(windows)]
pub fn error_box(title: &str, text: &str) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONERROR, MB_OK};
    let w = |s: &str| s.encode_utf16().chain(std::iter::once(0)).collect::<Vec<u16>>();
    let (t, m) = (w(title), w(text));
    unsafe { MessageBoxW(std::ptr::null_mut(), m.as_ptr(), t.as_ptr(), MB_OK | MB_ICONERROR) };
}

#[cfg(not(windows))]
pub fn error_box(title: &str, text: &str) {
    eprintln!("{title}: {text}");
}

/// Veriyi Windows DPAPI ile, yalnızca bu Windows kullanıcısının çözebileceği şekilde şifreler
/// (MVP.md §9.1: davet token'ı diskte düz metin durmaz).
#[cfg(windows)]
pub fn protect(data: &[u8]) -> Option<Vec<u8>> {
    use windows_sys::Win32::Security::Cryptography::{CryptProtectData, CRYPT_INTEGER_BLOB};
    let input = CRYPT_INTEGER_BLOB { cbData: data.len() as u32, pbData: data.as_ptr() as *mut u8 };
    let mut out = CRYPT_INTEGER_BLOB { cbData: 0, pbData: std::ptr::null_mut() };
    let ok = unsafe {
        CryptProtectData(&input, std::ptr::null(), std::ptr::null(), std::ptr::null(), std::ptr::null(), 0, &mut out)
    };
    take_blob(ok, out)
}

#[cfg(windows)]
pub fn unprotect(data: &[u8]) -> Option<Vec<u8>> {
    use windows_sys::Win32::Security::Cryptography::{CryptUnprotectData, CRYPT_INTEGER_BLOB};
    let input = CRYPT_INTEGER_BLOB { cbData: data.len() as u32, pbData: data.as_ptr() as *mut u8 };
    let mut out = CRYPT_INTEGER_BLOB { cbData: 0, pbData: std::ptr::null_mut() };
    let ok = unsafe {
        CryptUnprotectData(&input, std::ptr::null_mut(), std::ptr::null(), std::ptr::null(), std::ptr::null(), 0, &mut out)
    };
    take_blob(ok, out)
}

#[cfg(windows)]
fn take_blob(ok: i32, out: windows_sys::Win32::Security::Cryptography::CRYPT_INTEGER_BLOB) -> Option<Vec<u8>> {
    use windows_sys::Win32::Foundation::LocalFree;
    if ok == 0 || out.pbData.is_null() {
        return None;
    }
    let v = unsafe { std::slice::from_raw_parts(out.pbData, out.cbData as usize).to_vec() };
    unsafe { LocalFree(out.pbData as _) };
    Some(v)
}

#[cfg(not(windows))]
pub fn protect(data: &[u8]) -> Option<Vec<u8>> {
    Some(data.to_vec())
}

#[cfg(not(windows))]
pub fn unprotect(data: &[u8]) -> Option<Vec<u8>> {
    Some(data.to_vec())
}

/// Mevcut thread'i MMCSS "Pro Audio" görevine kaydeder (MVP.md §8).
/// İş yükü çok küçük olduğu için oyundan zaman çalmaz ama oyun CPU'yu doldurduğunda
/// ses thread'inin geç kalıp sesin kesilmesini önler.
#[cfg(windows)]
pub fn boost_audio_thread() -> bool {
    use windows_sys::Win32::System::Threading::AvSetMmThreadCharacteristicsW;
    let task: Vec<u16> = "Pro Audio\0".encode_utf16().collect();
    let mut index = 0u32;
    let h = unsafe { AvSetMmThreadCharacteristicsW(task.as_ptr(), &mut index) };
    !h.is_null()
}

#[cfg(not(windows))]
pub fn boost_audio_thread() -> bool {
    false
}

/// Süreç kaynak kullanımı ölçümü (MVP.md §3 hedefleri için).
pub struct ProcStats {
    last_cpu_100ns: u64,
    last_wall: std::time::Instant,
}

pub struct Sample {
    /// Son ölçümden beri, tek çekirdeğin yüzdesi olarak CPU.
    pub cpu_pct: f32,
    pub working_set_mb: f32,
    pub private_mb: f32,
}

impl ProcStats {
    pub fn new() -> Self {
        Self { last_cpu_100ns: cpu_time_100ns(), last_wall: std::time::Instant::now() }
    }

    pub fn sample(&mut self) -> Sample {
        let cpu = cpu_time_100ns();
        let now = std::time::Instant::now();
        let wall_100ns = now.duration_since(self.last_wall).as_nanos() as f64 / 100.0;
        let cpu_pct = if wall_100ns > 0.0 {
            (cpu.saturating_sub(self.last_cpu_100ns) as f64 / wall_100ns * 100.0) as f32
        } else {
            0.0
        };
        self.last_cpu_100ns = cpu;
        self.last_wall = now;
        let (ws, private) = memory_bytes();
        Sample {
            cpu_pct,
            working_set_mb: ws as f32 / 1_048_576.0,
            private_mb: private as f32 / 1_048_576.0,
        }
    }
}

#[cfg(windows)]
fn cpu_time_100ns() -> u64 {
    use windows_sys::Win32::Foundation::FILETIME;
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};
    let z = FILETIME { dwLowDateTime: 0, dwHighDateTime: 0 };
    let (mut c, mut e, mut k, mut u) = (z, z, z, z);
    let ok = unsafe { GetProcessTimes(GetCurrentProcess(), &mut c, &mut e, &mut k, &mut u) };
    if ok == 0 {
        return 0;
    }
    let f = |t: FILETIME| ((t.dwHighDateTime as u64) << 32) | t.dwLowDateTime as u64;
    f(k) + f(u)
}

#[cfg(windows)]
fn memory_bytes() -> (usize, usize) {
    use windows_sys::Win32::System::ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS};
    use windows_sys::Win32::System::Threading::GetCurrentProcess;
    let mut pmc: PROCESS_MEMORY_COUNTERS = unsafe { std::mem::zeroed() };
    pmc.cb = std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32;
    let ok = unsafe { GetProcessMemoryInfo(GetCurrentProcess(), &mut pmc, pmc.cb) };
    if ok == 0 {
        return (0, 0);
    }
    // PagefileUsage = Process Explorer'daki "Private Bytes".
    (pmc.WorkingSetSize, pmc.PagefileUsage)
}

#[cfg(not(windows))]
fn cpu_time_100ns() -> u64 {
    0
}

#[cfg(not(windows))]
fn memory_bytes() -> (usize, usize) {
    (0, 0)
}

#[cfg(all(test, windows))]
mod tests {
    #[test]
    fn dpapi_roundtrip() {
        let secret = b"corddis://1.2.3.4:443?fp=AB&token=xyz";
        let enc = super::protect(secret).unwrap();
        assert_ne!(enc.as_slice(), secret.as_slice());
        assert_eq!(super::unprotect(&enc).unwrap(), secret);
        assert!(super::unprotect(b"bozuk veri").is_none());
    }
}
