//! Native Windows dialogs, built on Task Dialogs so they match the system theme and DPI.

use soundhonter::engine::{Format, MixOptions};
use soundhonter::job::{Shared, Status};
use std::ptr::{null, null_mut};
use std::sync::atomic::Ordering;
use windows_sys::core::HRESULT;
use windows_sys::Win32::Foundation::{HWND, LPARAM, S_FALSE, S_OK, WPARAM};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Controls::*;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

const TITLE: &str = "Sound Honter";
const ICON_RESOURCE_ID: usize = 1;
const PROGRESS_RANGE: isize = 1000;
const WIDTH_DIALOG_UNITS: u32 = 260;

pub fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}

fn base_config(title: *const u16) -> TASKDIALOGCONFIG {
    // SAFETY: TASKDIALOGCONFIG is plain data, for which all-zero means "use the defaults".
    let mut config: TASKDIALOGCONFIG = unsafe { std::mem::zeroed() };
    config.cbSize = std::mem::size_of::<TASKDIALOGCONFIG>() as u32;
    config.pszWindowTitle = title;
    config.dwFlags = TDF_ALLOW_DIALOG_CANCELLATION;
    config.cxWidth = WIDTH_DIALOG_UNITS;
    config
}

/// The app's icon, from the exe's resources. Null if it has none, as in test builds.
pub fn app_icon() -> HICON {
    // SAFETY: loads a shared resource, which needs no cleanup.
    unsafe {
        LoadImageW(
            GetModuleHandleW(null()),
            ICON_RESOURCE_ID as *const u16,
            IMAGE_ICON,
            0,
            0,
            LR_DEFAULTSIZE | LR_SHARED,
        )
    }
}

unsafe fn set_window_icon(window: HWND) {
    let icon = app_icon();
    if !icon.is_null() {
        SendMessageW(window, WM_SETICON, ICON_BIG as WPARAM, icon as LPARAM);
        SendMessageW(window, WM_SETICON, ICON_SMALL as WPARAM, icon as LPARAM);
    }
}

unsafe extern "system" fn plain_callback(
    window: HWND,
    message: TASKDIALOG_NOTIFICATIONS,
    _wparam: WPARAM,
    _lparam: LPARAM,
    _data: isize,
) -> HRESULT {
    if message == TDN_CREATED {
        set_window_icon(window);
    }
    S_OK
}

pub enum Icon {
    None,
    Warning,
    Error,
}

/// Shows a message with optional extra buttons. Returns the id of the button clicked.
pub fn message(
    icon: Icon,
    heading: &str,
    body: &str,
    details: Option<&str>,
    buttons: &[(i32, &str)],
) -> i32 {
    let title = wide(TITLE);
    let heading = wide(heading);
    let body = wide(body);
    let details = details.map(wide);
    let show_details = wide("Show details");
    let labels: Vec<Vec<u16>> = buttons.iter().map(|(_, label)| wide(label)).collect();
    let button_defs: Vec<TASKDIALOG_BUTTON> = buttons
        .iter()
        .zip(&labels)
        .map(|((id, _), label)| TASKDIALOG_BUTTON {
            nButtonID: *id,
            pszButtonText: label.as_ptr(),
        })
        .collect();

    let mut config = base_config(title.as_ptr());
    config.pszMainInstruction = heading.as_ptr();
    config.pszContent = body.as_ptr();
    config.pfCallback = Some(plain_callback);
    config.dwCommonButtons = TDCBF_CLOSE_BUTTON;
    config.cButtons = button_defs.len() as u32;
    config.pButtons = button_defs.as_ptr();
    config.Anonymous1.pszMainIcon = match icon {
        Icon::None => null(),
        Icon::Warning => TD_WARNING_ICON,
        Icon::Error => TD_ERROR_ICON,
    };
    if let Some(details) = &details {
        config.pszExpandedInformation = details.as_ptr();
        config.pszCollapsedControlText = show_details.as_ptr();
    }

    let mut clicked = 0;
    // SAFETY: every pointer in `config` refers to a local that outlives this blocking call.
    let result = unsafe { TaskDialogIndirect(&config, &mut clicked, null_mut(), null_mut()) };
    if result == S_OK {
        clicked
    } else {
        IDCLOSE
    }
}

/// Asks how to mix a selection. Returns `None` if the user cancelled.
pub fn mix_options(file_count: usize, initial: MixOptions) -> Option<MixOptions> {
    const ID_MIX: i32 = 1100;
    const ID_FIRST_FORMAT: i32 = 1200;

    let title = wide(TITLE);
    let heading = wide(&format!("Mix {file_count} files"));
    let body = wide("Choose a format for the mix, and whether to even out its loudness.");
    let mix_label = wide("Mix");
    let normalize_label = wide("Normalize loudness");
    let format_labels: Vec<Vec<u16>> = Format::ALL
        .iter()
        .map(|format| wide(&format.extension().to_uppercase()))
        .collect();
    let radios: Vec<TASKDIALOG_BUTTON> = format_labels
        .iter()
        .enumerate()
        .map(|(i, label)| TASKDIALOG_BUTTON {
            nButtonID: ID_FIRST_FORMAT + i as i32,
            pszButtonText: label.as_ptr(),
        })
        .collect();
    let buttons = [TASKDIALOG_BUTTON {
        nButtonID: ID_MIX,
        pszButtonText: mix_label.as_ptr(),
    }];
    let initial_index = Format::ALL.iter().position(|f| *f == initial.format);

    let mut config = base_config(title.as_ptr());
    config.pszMainInstruction = heading.as_ptr();
    config.pszContent = body.as_ptr();
    config.pfCallback = Some(plain_callback);
    config.dwCommonButtons = TDCBF_CANCEL_BUTTON;
    config.cButtons = buttons.len() as u32;
    config.pButtons = buttons.as_ptr();
    config.nDefaultButton = ID_MIX;
    config.cRadioButtons = radios.len() as u32;
    config.pRadioButtons = radios.as_ptr();
    config.nDefaultRadioButton = ID_FIRST_FORMAT + initial_index.unwrap_or(0) as i32;
    config.pszVerificationText = normalize_label.as_ptr();
    if initial.normalize {
        config.dwFlags |= TDF_VERIFICATION_FLAG_CHECKED;
    }

    let (mut clicked, mut radio, mut normalize) = (0, 0, 0);
    // SAFETY: every pointer in `config` refers to a local that outlives this blocking call.
    let result = unsafe { TaskDialogIndirect(&config, &mut clicked, &mut radio, &mut normalize) };
    if result != S_OK || clicked != ID_MIX {
        return None;
    }
    let index = (radio - ID_FIRST_FORMAT) as usize;
    Some(MixOptions {
        format: Format::ALL.get(index).copied().unwrap_or(initial.format),
        normalize: normalize != 0,
    })
}

/// Shows a progress dialog until the work sets `finished`. Cancel sets the shared flag.
pub fn progress(shared: &Shared, initial_heading: &str) {
    let title = wide(TITLE);
    let heading = wide(initial_heading);
    // A placeholder reserves the line, since text can only be updated in elements that exist.
    let body = wide(" ");

    let mut config = base_config(title.as_ptr());
    config.dwFlags |= TDF_SHOW_PROGRESS_BAR | TDF_CALLBACK_TIMER;
    config.dwCommonButtons = TDCBF_CANCEL_BUTTON;
    config.pszMainInstruction = heading.as_ptr();
    config.pszContent = body.as_ptr();
    config.pfCallback = Some(progress_callback);
    config.lpCallbackData = shared as *const Shared as isize;

    // SAFETY: `shared` and the strings outlive this call, which blocks until the dialog closes.
    unsafe { TaskDialogIndirect(&config, null_mut(), null_mut(), null_mut()) };
}

unsafe extern "system" fn progress_callback(
    window: HWND,
    message: TASKDIALOG_NOTIFICATIONS,
    wparam: WPARAM,
    _lparam: LPARAM,
    data: isize,
) -> HRESULT {
    let shared = &*(data as *const Shared);
    match message {
        TDN_CREATED => {
            set_window_icon(window);
            let range = PROGRESS_RANGE << 16;
            SendMessageW(window, TDM_SET_PROGRESS_BAR_RANGE as u32, 0, range);
            S_OK
        }
        TDN_TIMER => {
            if shared.finished.load(Ordering::Acquire) {
                SendMessageW(window, TDM_CLICK_BUTTON as u32, IDCANCEL as WPARAM, 0);
            } else if !shared.cancel.load(Ordering::Relaxed) {
                let status: Status = shared.status.lock().unwrap().clone();
                show_status(window, &status);
            }
            S_OK
        }
        TDN_BUTTON_CLICKED if wparam == IDCANCEL as WPARAM => {
            if shared.finished.load(Ordering::Acquire) {
                return S_OK;
            }
            // Stay open until the work has stopped and cleaned up after itself.
            shared.cancel.store(true, Ordering::Relaxed);
            update_text(window, TDE_MAIN_INSTRUCTION, &wide("Cancelling…"));
            S_FALSE
        }
        _ => S_OK,
    }
}

unsafe fn show_status(window: HWND, status: &Status) {
    if !status.heading.is_empty() {
        update_text(window, TDE_MAIN_INSTRUCTION, &wide(&status.heading));
        update_text(window, TDE_CONTENT, &wide(&shorten(&status.detail)));
    }
    let position = (status.fraction * PROGRESS_RANGE as f64) as WPARAM;
    SendMessageW(window, TDM_SET_PROGRESS_BAR_POS as u32, position, 0);
}

unsafe fn update_text(window: HWND, element: TASKDIALOG_ELEMENTS, text: &[u16]) {
    SendMessageW(
        window,
        TDM_UPDATE_ELEMENT_TEXT as u32,
        element as WPARAM,
        text.as_ptr() as LPARAM,
    );
}

/// Keeps long names to one line, so the dialog doesn't change size as files go by.
fn shorten(text: &str) -> String {
    const LIMIT: usize = 58;
    if text.chars().count() <= LIMIT {
        return text.to_string();
    }
    text.chars().take(LIMIT - 1).chain(Some('…')).collect()
}
