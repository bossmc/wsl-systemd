use core::mem::MaybeUninit;

use windows::core::{s, PCSTR};
use windows::Win32::Foundation::{HANDLE, HWND, LPARAM, WPARAM};

use byteorder::{ByteOrder as _, BigEndian};

#[derive(thiserror::Error, Debug)]
enum Error {
    #[error("Windows API error: {0}")]
    Windows(#[from] windows::core::Error),
    #[error("No Pageant window found")]
    NoPageantWindow,
    #[error("Request too long")]
    RequestTooLong,
    #[error("Pageant rejected our request")]
    SendMessageFailed,
}

type Result<T, E = Error> = std::result::Result<T, E>;

#[derive(Debug, Clone)]
struct DroppableHandle(HANDLE);

impl std::ops::Drop for DroppableHandle {
    fn drop(&mut self) {
        if !self.0.is_invalid() {
            eprintln!("Closing {:?}", self.0);
            unsafe {
                windows::Win32::Foundation::CloseHandle(self.0).expect("can close valid handles");
            }
        }
    }
}

#[derive(Debug, Clone)]
struct ViewOfFile(windows::Win32::System::Memory::MEMORY_MAPPED_VIEW_ADDRESS);

impl ViewOfFile {
    fn as_slice(&mut self) -> &mut [MaybeUninit<u8>; 8192] {
        unsafe { &mut *self.0.Value.cast() }
    }
}

impl std::ops::Drop for ViewOfFile {
    fn drop(&mut self) {
        if !self.0.Value.is_null() {
            eprintln!("Unmapping {:?}", self.0);
            unsafe {
                windows::Win32::System::Memory::UnmapViewOfFile(self.0).expect("can unmap view of file");
            }
        }
    }
}

fn send_to_pageant(data: &[u8]) -> Result<Vec<u8>> {
    if data.len() >= 8192 {
        return Err(Error::RequestTooLong);
    }

    let window_handle = unsafe {
        windows::Win32::UI::WindowsAndMessaging::FindWindowA(s!("Pageant"), s!("Pageant"))
    };

    if window_handle.0 == 0 {
        return Err(Error::NoPageantWindow);
    }

    eprintln!("Found Pagent window: {:x?}", window_handle);

    let tid = unsafe { windows::Win32::System::Threading::GetCurrentThreadId() };
    let map_name = format!("PageantRequest{:x?}", tid);

    eprintln!("Map name is: {:?}", map_name);

    let map_pcstr_len = map_name.len() as u32 + 1; // Include nul-bytes
    let map_pcstr = std::ffi::CString::new(map_name).expect("map_name doesn't contain nul bytes");
    let map_pcstr = PCSTR(map_pcstr.as_ptr().cast());

    let file_mapping_handle = DroppableHandle(unsafe {
        windows::Win32::System::Memory::CreateFileMappingA(
            HWND(0),
            None,
            windows::Win32::System::Memory::PAGE_READWRITE,
            0,
            8192,
            map_pcstr,
        )
    }?);

    eprintln!("Created file mapping: {:?}", file_mapping_handle);

    let mut shm = ViewOfFile(unsafe {
        windows::Win32::System::Memory::MapViewOfFile(
            file_mapping_handle.0,
            windows::Win32::System::Memory::FILE_MAP_WRITE,
            0,
            0,
            0,
        )
    });

    eprintln!("Created view of file: {:?}", shm);
    let shm = shm.as_slice();

    unsafe { std::ptr::copy(data.as_ptr().cast(), (&mut shm[..]).as_mut_ptr(), data.len()) };

    let copy_data = windows::Win32::System::DataExchange::COPYDATASTRUCT {
        // https://github.com/Yasushi/putty/blob/31a2ad775f393aad1c31a983b0baea205d48e219/windows/winpgntc.c#L14
        dwData: 0x804e50ba,
        cbData: map_pcstr_len,
        lpData: map_pcstr.0.cast_mut().cast(),
    };

    eprintln!("COPYDATASTRUCT: {:?}", copy_data);

    let ret = unsafe {
        windows::Win32::UI::WindowsAndMessaging::SendMessageA(
            window_handle,
            windows::Win32::UI::WindowsAndMessaging::WM_COPYDATA,
            WPARAM(0),
            LPARAM(&copy_data as *const _ as isize),
        )
    };

    eprintln!("SendMessage(WM_COPYDATA) returned: {:?}", ret);

    if ret.0 == 0 {
        return Err(Error::SendMessageFailed);
    }

    let rsp_len = &shm[0..4];
    let rsp_len: &[u8] = unsafe { std::mem::transmute(rsp_len) };
    let rsp_len = BigEndian::read_u32(rsp_len) as usize;

    eprintln!("Response length is: {}", rsp_len);

    let mut rsp = Vec::with_capacity(rsp_len as usize);
    unsafe {
        rsp.extend_from_slice(std::mem::transmute(&shm[0..rsp_len + 4])); // Remember to include
                                                                          // the length field of
                                                                          // the response...
    }

    Ok(rsp)
}

fn main() {
    use std::io::{Write as _, Read as _};

    eprintln!("Starting up!");

    loop {
        let req = {
            let mut stdin = std::io::stdin().lock();
            let mut len_buf = [0;4];
            match stdin.read_exact(&mut len_buf) {
                Ok(_) => {}
                Err(e) => {
                    if e.kind() == std::io::ErrorKind::UnexpectedEof {
                        return;
                    } else {
                        panic!("stdin is unreadable");
                    }
                }
            }
            let req_len = BigEndian::read_u32(&len_buf);
            eprintln!("Request length: {}", req_len);

            let mut req = Vec::with_capacity(req_len as usize + 4);
            req.extend_from_slice(&len_buf);
            stdin.take(req_len as u64).read_to_end(&mut req).expect("should be able to read len bytes");

            req
        };

        eprintln!("Request: {:?}", req);

        let rsp = send_to_pageant(&req).unwrap();

        let mut stdout = std::io::stdout().lock();
        for chunk in rsp.chunks(16) {
            eprintln!("Response chunk: {:?}", chunk);
            stdout.write_all(&chunk).expect("writes to stdout can't fail");
            stdout.flush().expect("can flush stdout");
        }
    }
}
