//! The settings window: a dropdown for each setting, saved to settings.ini when you click Save.

use crate::ui;
use soundhonter::settings::*;
use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::ptr::{null, null_mut};
use windows_sys::Win32::Foundation::{
    GetLastError, ERROR_ALREADY_EXISTS, HWND, LPARAM, LRESULT, RECT, WPARAM,
};
use windows_sys::Win32::Graphics::Gdi::{
    CreateFontIndirectW, DeleteObject, GetMonitorInfoW, MonitorFromWindow, COLOR_BTNFACE, HBRUSH,
    HFONT, MONITORINFO, MONITOR_DEFAULTTONEAREST,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Threading::CreateMutexW;
use windows_sys::Win32::UI::HiDpi::{
    AdjustWindowRectExForDpi, GetDpiForWindow, SystemParametersInfoForDpi,
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::SetFocus;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

// Control styles and messages, spelled out because windows-sys types them inconsistently.
const CBS_DROPDOWNLIST: u32 = 0x0003;
const BS_PUSHBUTTON: u32 = 0x0000;
const BS_DEFPUSHBUTTON: u32 = 0x0001;
const ES_AUTOHSCROLL: u32 = 0x0080;
const SS_CENTERIMAGE: u32 = 0x0200;
const CB_ADDSTRING: u32 = 0x0143;
const CB_GETCURSEL: u32 = 0x0147;
const CB_SETCURSEL: u32 = 0x014E;
const EM_LIMITTEXT: u32 = 0x00C5;
const FW_SEMIBOLD: i32 = 600;

const ID_RESET: i32 = 100;
const ID_SUFFIX: i32 = 101;
const ID_FIRST_CHOICE: i32 = 200;

/// Layout, in pixels at 100% scaling.
const MARGIN: i32 = 16;
const LABEL_WIDTH: i32 = 170;
const FIELD_WIDTH: i32 = 270;
const GAP: i32 = 8;
const ROW_HEIGHT: i32 = 30;
const FIELD_HEIGHT: i32 = 23;
const HEADING_HEIGHT: i32 = 24;
const SECTION_GAP: i32 = 10;
const BUTTON_HEIGHT: i32 = 26;
const BUTTON_WIDTH: i32 = 88;
const RESET_WIDTH: i32 = 130;
const CLIENT_WIDTH: i32 = MARGIN + LABEL_WIDTH + GAP + FIELD_WIDTH + MARGIN;

/// A setting shown as a dropdown.
struct Choice {
    label: &'static str,
    options: Vec<&'static str>,
    get: fn(&Settings) -> usize,
    set: fn(&mut Settings, usize),
}

enum Item {
    Heading(&'static str),
    Choice(Choice),
    Suffix,
}

fn position<T: PartialEq>(all: &[T], value: T) -> usize {
    all.iter().position(|choice| *choice == value).unwrap_or(0)
}

fn items() -> Vec<Item> {
    macro_rules! choice {
        ($label:literal, $type:ident, $field:ident) => {
            Item::Choice(Choice {
                label: $label,
                options: $type::ALL.iter().map(|c| c.label()).collect(),
                get: |s| position($type::ALL, s.$field),
                set: |s, i| s.$field = $type::ALL[i],
            })
        };
    }
    vec![
        Item::Heading("Converting"),
        choice!("MP3 quality", Mp3Quality, mp3_quality),
        choice!("WAV bit depth", WavDepth, wav_depth),
        choice!("OGG and M4A quality", LossyQuality, lossy_quality),
        Item::Heading("All new files"),
        choice!("Sample rate", SampleRate, sample_rate),
        choice!("Channels", Channels, channels),
        choice!("Save new files", OutputFolder, output_folder),
        Item::Suffix,
        Item::Heading("Normalizing and mixing"),
        choice!("Loudness target", LoudnessTarget, loudness_target),
        choice!("Peak ceiling", PeakCeiling, peak_ceiling),
        Item::Heading("Trimming"),
        choice!("Trim silence", SilenceThreshold, silence_threshold),
    ]
}

/// A control and where it goes, in pixels at 100% scaling.
struct Placed {
    window: HWND,
    rect: (i32, i32, i32, i32),
    bold: bool,
}

struct State {
    data_dir: PathBuf,
    items: Vec<Item>,
    /// Each dropdown, with the index of its item.
    choices: Vec<(HWND, usize)>,
    suffix: HWND,
    placed: Vec<Placed>,
    client_height: i32,
    fonts: Fonts,
}

struct Fonts {
    normal: HFONT,
    bold: HFONT,
}

impl Fonts {
    /// The system's message font, at the window's scaling.
    fn new(dpi: u32) -> Fonts {
        // SAFETY: plain data, filled in by the call below.
        unsafe {
            let mut metrics: NONCLIENTMETRICSW = std::mem::zeroed();
            metrics.cbSize = std::mem::size_of::<NONCLIENTMETRICSW>() as u32;
            SystemParametersInfoForDpi(
                SPI_GETNONCLIENTMETRICS,
                metrics.cbSize,
                &mut metrics as *mut _ as *mut _,
                0,
                dpi,
            );
            let mut font = metrics.lfMessageFont;
            let normal = CreateFontIndirectW(&font);
            font.lfWeight = FW_SEMIBOLD;
            let bold = CreateFontIndirectW(&font);
            Fonts { normal, bold }
        }
    }
}

impl Drop for Fonts {
    fn drop(&mut self) {
        // SAFETY: the fonts are ours, and no longer selected into any window once replaced.
        unsafe {
            DeleteObject(self.normal);
            DeleteObject(self.bold);
        }
    }
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
}

fn scale(value: i32, dpi: u32) -> i32 {
    value * dpi as i32 / 96
}

/// Shows the settings window and returns when it's closed. If it's already open, as happens when
/// Settings is chosen with several files selected, brings that one forward instead.
pub fn show(data_dir: &Path) {
    // SAFETY: standard window setup; every pointer passed refers to a live local.
    unsafe {
        let class_name = ui::wide("SoundHonterSettings");
        let mutex_name = ui::wide("Local\\SoundHonterSettings");
        let _mutex = CreateMutexW(null(), 1, mutex_name.as_ptr());
        if GetLastError() == ERROR_ALREADY_EXISTS {
            // The other window may still be appearing.
            for _ in 0..50 {
                let existing = FindWindowW(class_name.as_ptr(), null());
                if !existing.is_null() {
                    ShowWindow(existing, SW_RESTORE);
                    SetForegroundWindow(existing);
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            return;
        }
        let instance = GetModuleHandleW(null());
        let class = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            style: 0,
            lpfnWndProc: Some(window_proc),
            cbClsExtra: 0,
            cbWndExtra: 0,
            hInstance: instance,
            hIcon: ui::app_icon(),
            hCursor: LoadCursorW(null_mut(), IDC_ARROW),
            hbrBackground: (COLOR_BTNFACE + 1) as usize as HBRUSH,
            lpszMenuName: null(),
            lpszClassName: class_name.as_ptr(),
            hIconSm: ui::app_icon(),
        };
        RegisterClassExW(&class);

        let title = ui::wide("Sound Honter Settings");
        let style = WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX;
        let window = CreateWindowExW(
            WS_EX_CONTROLPARENT,
            class_name.as_ptr(),
            title.as_ptr(),
            style,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            null_mut(),
            null_mut(),
            instance,
            null(),
        );
        if window.is_null() {
            return;
        }

        let dpi = GetDpiForWindow(window);
        let mut state = build(window, data_dir);
        state.fonts = Fonts::new(dpi);
        load_values(&state, &Settings::load(data_dir));
        apply_layout(&state, dpi);

        // Size the window to fit, centered on its screen.
        let mut frame = RECT {
            left: 0,
            top: 0,
            right: scale(CLIENT_WIDTH, dpi),
            bottom: scale(state.client_height, dpi),
        };
        AdjustWindowRectExForDpi(&mut frame, style, 0, WS_EX_CONTROLPARENT, dpi);
        let (width, height) = (frame.right - frame.left, frame.bottom - frame.top);
        let mut monitor: MONITORINFO = std::mem::zeroed();
        monitor.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
        GetMonitorInfoW(
            MonitorFromWindow(window, MONITOR_DEFAULTTONEAREST),
            &mut monitor,
        );
        let work = monitor.rcWork;
        let x = work.left + (work.right - work.left - width) / 2;
        let y = work.top + (work.bottom - work.top - height) / 2;
        SetWindowPos(window, null_mut(), x, y, width, height, SWP_NOZORDER);

        let first = state.choices.first().map(|(control, _)| *control);
        STATE.with(|cell| *cell.borrow_mut() = Some(state));
        ShowWindow(window, SW_SHOW);
        if let Some(first) = first {
            SetFocus(first);
        }

        let mut message: MSG = std::mem::zeroed();
        while GetMessageW(&mut message, null_mut(), 0, 0) > 0 {
            if IsDialogMessageW(window, &message) == 0 {
                TranslateMessage(&message);
                DispatchMessageW(&message);
            }
        }
        STATE.with(|cell| cell.borrow_mut().take());
    }
}

/// Creates every control. Positions and fonts are applied separately, so they can follow
/// the window between screens with different scaling.
unsafe fn build(window: HWND, data_dir: &Path) -> State {
    let mut state = State {
        data_dir: data_dir.to_path_buf(),
        items: items(),
        choices: Vec::new(),
        suffix: null_mut(),
        placed: Vec::new(),
        client_height: 0,
        fonts: Fonts {
            normal: null_mut(),
            bold: null_mut(),
        },
    };
    let field_x = MARGIN + LABEL_WIDTH + GAP;
    let mut y = MARGIN;
    let mut next_id = ID_FIRST_CHOICE;
    let mut placed = Vec::new();
    let mut label = |text: &str, y: i32, bold: bool| {
        let height = if bold { HEADING_HEIGHT } else { FIELD_HEIGHT };
        let control = child(window, "STATIC", text, SS_CENTERIMAGE, 0);
        let width = if bold {
            CLIENT_WIDTH - 2 * MARGIN
        } else {
            LABEL_WIDTH
        };
        placed.push(Placed {
            window: control,
            rect: (MARGIN, y, width, height),
            bold,
        });
    };
    for (index, item) in state.items.iter().enumerate() {
        match item {
            Item::Heading(text) => {
                if index > 0 {
                    y += SECTION_GAP;
                }
                label(text, y, true);
                y += HEADING_HEIGHT + 2;
            }
            Item::Choice(choice) => {
                label(choice.label, y, false);
                let control = child(
                    window,
                    "COMBOBOX",
                    "",
                    CBS_DROPDOWNLIST | WS_VSCROLL | WS_TABSTOP,
                    next_id,
                );
                next_id += 1;
                for option in &choice.options {
                    let text = ui::wide(option);
                    SendMessageW(control, CB_ADDSTRING, 0, text.as_ptr() as LPARAM);
                }
                // A dropdown's height includes its open list.
                let list_height = FIELD_HEIGHT * (choice.options.len() as i32 + 2);
                state.placed.push(Placed {
                    window: control,
                    rect: (field_x, y, FIELD_WIDTH, list_height),
                    bold: false,
                });
                state.choices.push((control, index));
                y += ROW_HEIGHT;
            }
            Item::Suffix => {
                label("Add to file names", y, false);
                let control = child(window, "EDIT", "", ES_AUTOHSCROLL | WS_TABSTOP, ID_SUFFIX);
                SetWindowLongPtrW(
                    control,
                    GWL_EXSTYLE,
                    (WS_EX_CLIENTEDGE | GetWindowLongPtrW(control, GWL_EXSTYLE) as u32) as isize,
                );
                SendMessageW(control, EM_LIMITTEXT, 40, 0);
                state.placed.push(Placed {
                    window: control,
                    rect: (field_x, y, FIELD_WIDTH, FIELD_HEIGHT),
                    bold: false,
                });
                state.suffix = control;
                y += ROW_HEIGHT;
            }
        }
    }
    state.placed.extend(placed);

    y += SECTION_GAP + 4;
    let buttons = [
        (
            ID_RESET,
            "Reset to defaults",
            MARGIN,
            RESET_WIDTH,
            BS_PUSHBUTTON,
        ),
        (
            IDOK,
            "Save",
            CLIENT_WIDTH - MARGIN - 2 * BUTTON_WIDTH - GAP,
            BUTTON_WIDTH,
            BS_DEFPUSHBUTTON,
        ),
        (
            IDCANCEL,
            "Cancel",
            CLIENT_WIDTH - MARGIN - BUTTON_WIDTH,
            BUTTON_WIDTH,
            BS_PUSHBUTTON,
        ),
    ];
    for (id, text, x, width, style) in buttons {
        let control = child(window, "BUTTON", text, style | WS_TABSTOP, id);
        state.placed.push(Placed {
            window: control,
            rect: (x, y, width, BUTTON_HEIGHT),
            bold: false,
        });
    }
    state.client_height = y + BUTTON_HEIGHT + MARGIN;
    state
}

unsafe fn child(parent: HWND, class: &str, text: &str, style: u32, id: i32) -> HWND {
    let class = ui::wide(class);
    let text = ui::wide(text);
    CreateWindowExW(
        0,
        class.as_ptr(),
        text.as_ptr(),
        WS_CHILD | WS_VISIBLE | style,
        0,
        0,
        0,
        0,
        parent,
        id as isize as HMENU,
        GetModuleHandleW(null()),
        null(),
    )
}

unsafe fn apply_layout(state: &State, dpi: u32) {
    for placed in &state.placed {
        let (x, y, width, height) = placed.rect;
        MoveWindow(
            placed.window,
            scale(x, dpi),
            scale(y, dpi),
            scale(width, dpi),
            scale(height, dpi),
            1,
        );
        let font = if placed.bold {
            state.fonts.bold
        } else {
            state.fonts.normal
        };
        SendMessageW(placed.window, WM_SETFONT, font as WPARAM, 1);
    }
}

unsafe fn load_values(state: &State, settings: &Settings) {
    for (control, index) in &state.choices {
        if let Item::Choice(choice) = &state.items[*index] {
            SendMessageW(*control, CB_SETCURSEL, (choice.get)(settings), 0);
        }
    }
    let suffix = ui::wide(&settings.name_suffix);
    SetWindowTextW(state.suffix, suffix.as_ptr());
}

unsafe fn read_values(state: &State) -> Settings {
    let mut settings = Settings::default();
    for (control, index) in &state.choices {
        if let Item::Choice(choice) = &state.items[*index] {
            let selected = SendMessageW(*control, CB_GETCURSEL, 0, 0);
            if selected >= 0 && (selected as usize) < choice.options.len() {
                (choice.set)(&mut settings, selected as usize);
            }
        }
    }
    let mut buffer = [0u16; 64];
    let length = GetWindowTextW(state.suffix, buffer.as_mut_ptr(), buffer.len() as i32);
    let text = String::from_utf16_lossy(&buffer[..length.max(0) as usize]);
    settings.name_suffix = clean_suffix(&text);
    settings
}

unsafe extern "system" fn window_proc(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        WM_COMMAND => {
            let id = (wparam & 0xFFFF) as i32;
            match id {
                IDOK => {
                    let saved = with_state(|state| {
                        let settings = read_values(state);
                        settings.save(&state.data_dir)
                    });
                    match saved {
                        Some(Err(error)) => {
                            let heading = "Sound Honter couldn't save your settings";
                            ui::message(ui::Icon::Error, heading, &error.to_string(), None, &[]);
                        }
                        _ => {
                            DestroyWindow(window);
                        }
                    }
                    0
                }
                IDCANCEL => {
                    DestroyWindow(window);
                    0
                }
                ID_RESET => {
                    with_state(|state| load_values(state, &Settings::default()));
                    0
                }
                _ => DefWindowProcW(window, message, wparam, lparam),
            }
        }
        WM_DPICHANGED => {
            let dpi = (wparam & 0xFFFF) as u32;
            with_state_mut(|state| {
                // The old fonts stay alive until the controls have moved on to the new ones.
                let _old = std::mem::replace(&mut state.fonts, Fonts::new(dpi));
                apply_layout(state, dpi);
            });
            let suggested = &*(lparam as *const RECT);
            SetWindowPos(
                window,
                null_mut(),
                suggested.left,
                suggested.top,
                suggested.right - suggested.left,
                suggested.bottom - suggested.top,
                SWP_NOZORDER | SWP_NOACTIVATE,
            );
            0
        }
        WM_CLOSE => {
            DestroyWindow(window);
            0
        }
        WM_DESTROY => {
            PostQuitMessage(0);
            0
        }
        _ => DefWindowProcW(window, message, wparam, lparam),
    }
}

/// Runs `action` with the window's state, unless it's already in use further up the stack:
/// controls send their own notifications back to the window while they're being changed.
fn with_state<R>(action: impl FnOnce(&State) -> R) -> Option<R> {
    STATE.with(|cell| cell.try_borrow().ok()?.as_ref().map(action))
}

fn with_state_mut(action: impl FnOnce(&mut State)) {
    STATE.with(|cell| {
        if let Ok(mut state) = cell.try_borrow_mut() {
            if let Some(state) = state.as_mut() {
                action(state);
            }
        }
    });
}
