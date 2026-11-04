//! A small HTTP client on WinHTTP.
//!
//! Windows brings its own TLS, certificates and proxy settings along, so the
//! program needs no copy of them and follows the system when they change.
//! Redirects are followed, except from https down to http.

use std::ffi::c_void;
use std::io;
use std::ptr;

use windows_sys::core::PCWSTR;
use windows_sys::Win32::Networking::WinHttp::{
    WinHttpAddRequestHeaders, WinHttpCloseHandle, WinHttpConnect, WinHttpOpen, WinHttpOpenRequest,
    WinHttpQueryDataAvailable, WinHttpQueryHeaders, WinHttpReadData, WinHttpReceiveResponse,
    WinHttpSendRequest, WinHttpSetTimeouts, WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY,
    WINHTTP_ADDREQ_FLAG_ADD, WINHTTP_FLAG_SECURE, WINHTTP_QUERY_CONTENT_LENGTH,
    WINHTTP_QUERY_FLAG_NUMBER, WINHTTP_QUERY_STATUS_CODE,
};

const RESOLVE_TIMEOUT_MS: i32 = 10_000;
const CONNECT_TIMEOUT_MS: i32 = 10_000;
const SEND_TIMEOUT_MS: i32 = 15_000;
const RECEIVE_TIMEOUT_MS: i32 = 30_000;
const CHUNK_BYTES: usize = 64 * 1024;
const HTTP_PORT: u16 = 80;
const HTTPS_PORT: u16 = 443;

/// Takes a piece of the body, along with its full length if the server gave one.
pub type OnData<'a> = dyn FnMut(&[u8], Option<u64>) -> io::Result<()> + 'a;

/// A GET request.
#[derive(Clone, Copy, Debug)]
pub struct Request<'a> {
    pub url: &'a str,
    pub user_agent: &'a str,
    /// The `Accept` header, if the server cares.
    pub accept: Option<&'a str>,
}

/// Closes a WinHTTP handle however the request ends.
struct Handle(*mut c_void);

impl Handle {
    fn new(raw: *mut c_void) -> io::Result<Self> {
        if raw.is_null() {
            Err(io::Error::last_os_error())
        } else {
            Ok(Self(raw))
        }
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        unsafe { WinHttpCloseHandle(self.0) };
    }
}

#[derive(Debug, PartialEq, Eq)]
struct Target {
    secure: bool,
    host: String,
    port: u16,
    path: String,
}

fn parse_url(url: &str) -> io::Result<Target> {
    let bad = || io::Error::new(io::ErrorKind::InvalidInput, format!("bad url: {url}"));
    let (secure, rest) = match (url.strip_prefix("https://"), url.strip_prefix("http://")) {
        (Some(rest), _) => (true, rest),
        (None, Some(rest)) => (false, rest),
        (None, None) => return Err(bad()),
    };
    let (authority, path) = match rest.find('/') {
        Some(slash) => rest.split_at(slash),
        None => (rest, "/"),
    };
    let default_port = if secure { HTTPS_PORT } else { HTTP_PORT };
    let (host, port) = match authority.split_once(':') {
        Some((host, port)) => (host, port.parse().map_err(|_| bad())?),
        None => (authority, default_port),
    };
    if host.is_empty() {
        return Err(bad());
    }
    Ok(Target {
        secure,
        host: host.to_string(),
        port,
        path: path.to_string(),
    })
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}

fn check(succeeded: i32) -> io::Result<()> {
    if succeeded == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

fn header_number(request: &Handle, header: u32) -> Option<u32> {
    let mut value = 0u32;
    let mut size = size_of::<u32>() as u32;
    let read = unsafe {
        WinHttpQueryHeaders(
            request.0,
            header | WINHTTP_QUERY_FLAG_NUMBER,
            ptr::null(),
            &mut value as *mut u32 as *mut c_void,
            &mut size,
            ptr::null_mut(),
        )
    };
    (read != 0).then_some(value)
}

/// Sends a request and returns the final status. The body of a 200 goes to
/// `on_data` piece by piece.
pub fn get(request: &Request, on_data: &mut OnData) -> io::Result<u32> {
    let target = parse_url(request.url)?;

    let agent = wide(request.user_agent);
    let session = Handle::new(unsafe {
        WinHttpOpen(
            agent.as_ptr() as PCWSTR,
            WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY,
            ptr::null(),
            ptr::null(),
            0,
        )
    })?;
    check(unsafe {
        WinHttpSetTimeouts(
            session.0,
            RESOLVE_TIMEOUT_MS,
            CONNECT_TIMEOUT_MS,
            SEND_TIMEOUT_MS,
            RECEIVE_TIMEOUT_MS,
        )
    })?;

    let host = wide(&target.host);
    let connection =
        Handle::new(unsafe { WinHttpConnect(session.0, host.as_ptr() as PCWSTR, target.port, 0) })?;

    let verb = wide("GET");
    let path = wide(&target.path);
    let flags = if target.secure { WINHTTP_FLAG_SECURE } else { 0 };
    let exchange = Handle::new(unsafe {
        WinHttpOpenRequest(
            connection.0,
            verb.as_ptr() as PCWSTR,
            path.as_ptr() as PCWSTR,
            ptr::null(),
            ptr::null(),
            ptr::null(),
            flags,
        )
    })?;

    if let Some(accept) = request.accept {
        let header = wide(&format!("Accept: {accept}"));
        check(unsafe {
            WinHttpAddRequestHeaders(
                exchange.0,
                header.as_ptr() as PCWSTR,
                (header.len() - 1) as u32,
                WINHTTP_ADDREQ_FLAG_ADD,
            )
        })?;
    }

    check(unsafe { WinHttpSendRequest(exchange.0, ptr::null(), 0, ptr::null(), 0, 0, 0) })?;
    check(unsafe { WinHttpReceiveResponse(exchange.0, ptr::null_mut()) })?;

    let status = header_number(&exchange, WINHTTP_QUERY_STATUS_CODE)
        .ok_or_else(|| io::Error::other("no status in the answer"))?;
    if status == 200 {
        let length = header_number(&exchange, WINHTTP_QUERY_CONTENT_LENGTH).map(u64::from);
        read_body(&exchange, length, on_data)?;
    }
    Ok(status)
}

fn read_body(exchange: &Handle, length: Option<u64>, on_data: &mut OnData) -> io::Result<()> {
    let mut buffer = vec![0u8; CHUNK_BYTES];
    loop {
        let mut available = 0u32;
        check(unsafe { WinHttpQueryDataAvailable(exchange.0, &mut available) })?;
        if available == 0 {
            return Ok(());
        }
        let wanted = (available as usize).min(buffer.len()) as u32;
        let mut read = 0u32;
        check(unsafe {
            WinHttpReadData(
                exchange.0,
                buffer.as_mut_ptr() as *mut c_void,
                wanted,
                &mut read,
            )
        })?;
        if read == 0 {
            return Ok(());
        }
        on_data(&buffer[..read as usize], length)?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;

    /// Answers each request with the text that goes with its first line.
    fn serve(answers: Vec<(&'static str, String)>) -> (u16, thread::JoinHandle<Vec<String>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = thread::spawn(move || {
            let mut heads = Vec::new();
            for _ in 0..answers.len() {
                let (mut stream, _) = listener.accept().unwrap();
                let mut buffer = [0u8; 2048];
                let read = stream.read(&mut buffer).unwrap();
                let head = String::from_utf8_lossy(&buffer[..read]).to_string();
                let answer = answers
                    .iter()
                    .find(|(start, _)| head.starts_with(start))
                    .map_or("HTTP/1.1 500 No\r\nContent-Length: 0\r\n\r\n", |(_, answer)| answer);
                stream.write_all(answer.as_bytes()).unwrap();
                heads.push(head);
            }
            heads
        });
        (port, server)
    }

    fn fetch(url: &str, accept: Option<&str>) -> io::Result<(u32, Vec<u8>, Option<u64>)> {
        let mut body = Vec::new();
        let mut total = None;
        let request = Request {
            url,
            user_agent: "ilumx-test",
            accept,
        };
        let status = get(&request, &mut |data, length| {
            body.extend_from_slice(data);
            total = length;
            Ok(())
        })?;
        Ok((status, body, total))
    }

    fn ok(body: &str) -> String {
        format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
    }

    #[test]
    fn splits_urls_into_their_parts() {
        let target = parse_url("https://api.github.com/repos/a/b").unwrap();
        assert_eq!(
            target,
            Target {
                secure: true,
                host: "api.github.com".to_string(),
                port: 443,
                path: "/repos/a/b".to_string(),
            }
        );
        let local = parse_url("http://127.0.0.1:8123").unwrap();
        assert_eq!((local.secure, local.port), (false, 8123));
        assert_eq!(local.path, "/");
    }

    #[test]
    fn refuses_what_is_no_web_address() {
        assert!(parse_url("ftp://example.com/a").is_err());
        assert!(parse_url("https:///a").is_err());
        assert!(parse_url("http://host:port/a").is_err());
    }

    #[test]
    fn hands_over_the_body_with_its_length() {
        let (port, server) = serve(vec![("GET /file", ok("hello"))]);
        let url = format!("http://127.0.0.1:{port}/file");
        let (status, body, total) = fetch(&url, None).unwrap();
        assert_eq!(status, 200);
        assert_eq!(body, b"hello");
        assert_eq!(total, Some(5));
        server.join().unwrap();
    }

    #[test]
    fn sends_who_is_asking_and_what_it_accepts() {
        let (port, server) = serve(vec![("GET /", ok(""))]);
        let url = format!("http://127.0.0.1:{port}/");
        fetch(&url, Some("application/json")).unwrap();
        let heads = server.join().unwrap();
        assert!(heads[0].contains("User-Agent: ilumx-test"), "{}", heads[0]);
        assert!(heads[0].contains("Accept: application/json"), "{}", heads[0]);
    }

    #[test]
    fn a_missing_page_gives_its_status_and_no_body() {
        let answer = "HTTP/1.1 404 Not Found\r\nContent-Length: 4\r\nConnection: close\r\n\r\nnope";
        let (port, server) = serve(vec![("GET /", answer.to_string())]);
        let url = format!("http://127.0.0.1:{port}/");
        let (status, body, _) = fetch(&url, None).unwrap();
        assert_eq!(status, 404);
        assert!(body.is_empty());
        server.join().unwrap();
    }

    #[test]
    fn follows_a_redirect_the_way_release_files_need() {
        let redirect = "HTTP/1.1 302 Found\r\nLocation: /final\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
        let (port, server) = serve(vec![
            ("GET /asset", redirect.to_string()),
            ("GET /final", ok("the file")),
        ]);
        let url = format!("http://127.0.0.1:{port}/asset");
        let (status, body, _) = fetch(&url, None).unwrap();
        assert_eq!(status, 200);
        assert_eq!(body, b"the file");
        server.join().unwrap();
    }

    #[test]
    fn a_failure_while_saving_stops_the_transfer() {
        let (port, server) = serve(vec![("GET /", ok("hello"))]);
        let url = format!("http://127.0.0.1:{port}/");
        let request = Request {
            url: &url,
            user_agent: "ilumx-test",
            accept: None,
        };
        let result = get(&request, &mut |_, _| Err(io::Error::other("disk full")));
        assert!(result.is_err());
        server.join().unwrap();
    }

    #[test]
    fn nobody_listening_is_an_error() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        assert!(fetch(&format!("http://127.0.0.1:{port}/"), None).is_err());
    }
}
