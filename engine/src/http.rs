//! HTTPS through WinHTTP: a POST that uploads telemetry, and a GET that reads an export back.
//!
//! This is the whole HTTP client -- no redirects, no connection reuse, no cookies. The engine that
//! ships uses `post` and nothing else; `get` exists for `likhi-report pull`, an operator tool that
//! reads collected telemetry back out of the ingest endpoint.
//!
//! WinHTTP rather than a Rust TLS stack because it is already on the machine. It uses the system
//! certificate store, which means a managed office network's proxy and TLS inspection work without
//! configuration -- exactly the environment the pilot runs in -- and it keeps rustls and ring out
//! of a binary shipped to end users. The Python it replaces used `urllib`, which also goes through
//! the OS trust store, so this preserves that behaviour rather than introducing a second opinion
//! about which certificates are acceptable.

/// Upload budget: the Python used a single 10 s timeout, and these are the four WinHTTP stages that
/// make it up. A dead network then costs 10 s rather than hanging a background thread for the life
/// of the process.
#[cfg(windows)]
const UPLOAD_TIMEOUT_MS: u32 = 10_000;

/// A whole pilot export in one response, so this is generous where the upload path is not.
#[cfg(windows)]
pub const EXPORT_TIMEOUT_MS: u32 = 120_000;

/// Refuse a response larger than this rather than growing a buffer until the machine swaps. The
/// server caps its own export well below this; the limit is here so a wrong URL answering with
/// something enormous fails fast.
#[cfg(windows)]
const MAX_RESPONSE: usize = 256 * 1024 * 1024;

#[cfg(windows)]
pub fn post(url: &str, headers: &[(&str, String)], body: &[u8]) -> Result<(), String> {
    request("POST", url, headers, body, UPLOAD_TIMEOUT_MS, false).map(|_| ())
}

/// GET returning the response body. Used by `likhi-report pull`.
#[cfg(windows)]
pub fn get(url: &str, headers: &[(&str, String)], timeout_ms: u32) -> Result<Vec<u8>, String> {
    request("GET", url, headers, &[], timeout_ms, true)
}

#[cfg(windows)]
fn request(
    method: &str,
    url: &str,
    headers: &[(&str, String)],
    body: &[u8],
    timeout_ms: u32,
    read_body: bool,
) -> Result<Vec<u8>, String> {
    use windows::core::PCWSTR;
    use windows::Win32::Networking::WinHttp::*;

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    /// Closes a WinHTTP handle when it goes out of scope. There are many early returns below and
    /// leaking a session handle per failed upload would be a slow leak in a process that runs for
    /// weeks.
    struct Handle(*mut core::ffi::c_void);
    impl Drop for Handle {
        fn drop(&mut self) {
            if !self.0.is_null() {
                unsafe {
                    let _ = WinHttpCloseHandle(self.0);
                }
            }
        }
    }

    let (scheme, rest) = url.split_once("://").ok_or("url has no scheme")?;
    let secure = match scheme {
        "https" => true,
        "http" => false,
        other => return Err(format!("unsupported scheme {other}")),
    };
    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) => (h, p.parse::<u16>().map_err(|_| "bad port")?),
        None => (authority, if secure { 443 } else { 80 }),
    };

    unsafe {
        let session = Handle(WinHttpOpen(
            PCWSTR(wide("Likhi/1").as_ptr()),
            WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY,
            PCWSTR::null(),
            PCWSTR::null(),
            0,
        ));
        if session.0.is_null() {
            return Err(format!("WinHttpOpen: {}", std::io::Error::last_os_error()));
        }
        let t = timeout_ms as i32;
        let _ = WinHttpSetTimeouts(session.0, t, t, t, t);

        let connect = Handle(WinHttpConnect(session.0, PCWSTR(wide(host).as_ptr()), port, 0));
        if connect.0.is_null() {
            return Err(format!("WinHttpConnect: {}", std::io::Error::last_os_error()));
        }

        let flags = if secure { WINHTTP_FLAG_SECURE } else { WINHTTP_OPEN_REQUEST_FLAGS(0) };
        let request = Handle(WinHttpOpenRequest(
            connect.0,
            PCWSTR(wide(method).as_ptr()),
            PCWSTR(wide(path).as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            std::ptr::null_mut(),
            flags,
        ));
        if request.0.is_null() {
            return Err(format!("WinHttpOpenRequest: {}", std::io::Error::last_os_error()));
        }

        if !headers.is_empty() {
            let joined: String = headers.iter().map(|(k, v)| format!("{k}: {v}\r\n")).collect();
            let head = wide(&joined);
            // -1 length means "the string is null-terminated"; the trailing NUL from `wide` is not
            // part of the header block.
            if WinHttpAddRequestHeaders(request.0, &head[..head.len() - 1], WINHTTP_ADDREQ_FLAG_ADD)
                .is_err()
            {
                return Err(format!(
                    "WinHttpAddRequestHeaders: {}",
                    std::io::Error::last_os_error()
                ));
            }
        }

        let body_ptr = if body.is_empty() {
            None
        } else {
            Some(body.as_ptr() as *const core::ffi::c_void)
        };
        if WinHttpSendRequest(
            request.0,
            None,
            body_ptr,
            body.len() as u32,
            body.len() as u32,
            0,
        )
        .is_err()
        {
            return Err(format!("WinHttpSendRequest: {}", std::io::Error::last_os_error()));
        }
        if WinHttpReceiveResponse(request.0, std::ptr::null_mut()).is_err() {
            return Err(format!(
                "WinHttpReceiveResponse: {}",
                std::io::Error::last_os_error()
            ));
        }

        let mut status: u32 = 0;
        let mut len = std::mem::size_of::<u32>() as u32;
        if WinHttpQueryHeaders(
            request.0,
            WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
            PCWSTR::null(),
            Some(&mut status as *mut u32 as *mut core::ffi::c_void),
            &mut len,
            std::ptr::null_mut(),
        )
        .is_err()
        {
            return Err(format!("WinHttpQueryHeaders: {}", std::io::Error::last_os_error()));
        }
        if !(200..300).contains(&status) {
            return Err(format!("endpoint returned {status}"));
        }

        if !read_body {
            return Ok(Vec::new());
        }

        // Read until a query reports nothing available. WinHTTP hands over one buffered block at a
        // time, so a multi-megabyte export arrives across many iterations.
        let mut out: Vec<u8> = Vec::new();
        loop {
            let mut avail: u32 = 0;
            if WinHttpQueryDataAvailable(request.0, &mut avail).is_err() {
                return Err(format!(
                    "WinHttpQueryDataAvailable: {}",
                    std::io::Error::last_os_error()
                ));
            }
            if avail == 0 {
                break;
            }
            if out.len() + avail as usize > MAX_RESPONSE {
                return Err(format!("response exceeds {MAX_RESPONSE} bytes"));
            }
            let start = out.len();
            out.resize(start + avail as usize, 0);
            let mut read: u32 = 0;
            if WinHttpReadData(
                request.0,
                out.as_mut_ptr().add(start) as *mut core::ffi::c_void,
                avail,
                &mut read,
            )
            .is_err()
            {
                return Err(format!("WinHttpReadData: {}", std::io::Error::last_os_error()));
            }
            // A short read is normal; drop the unfilled tail. A zero read means the peer stopped
            // early, and looping again would spin forever.
            out.truncate(start + read as usize);
            if read == 0 {
                break;
            }
        }
        Ok(out)
    }
}

#[cfg(not(windows))]
pub const EXPORT_TIMEOUT_MS: u32 = 120_000;

#[cfg(not(windows))]
pub fn post(_url: &str, _headers: &[(&str, String)], _body: &[u8]) -> Result<(), String> {
    Err("HTTP upload is implemented only on Windows".into())
}

#[cfg(not(windows))]
pub fn get(_url: &str, _headers: &[(&str, String)], _timeout_ms: u32) -> Result<Vec<u8>, String> {
    Err("HTTP download is implemented only on Windows".into())
}
