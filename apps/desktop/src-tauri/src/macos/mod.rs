//! AppKit glue: non-activating panels, capture hiding and notch geometry.
//!
//! This is the only module in the workspace allowed to use `unsafe`. Every block states why it is
//! sound. All functions here must run on the main thread.

#![allow(unsafe_code)]

mod geometry;

use objc2::runtime::{AnyClass, AnyObject};
use objc2::{ClassType, MainThreadMarker, MainThreadOnly, define_class};
use objc2_app_kit::{
    NSPanel, NSScreen, NSWindow, NSWindowCollectionBehavior, NSWindowSharingType,
    NSWindowStyleMask, NSWorkspace,
};
use objc2_foundation::{NSPoint, NSRect, NSSize, NSString, NSURL};
use tauri::WebviewWindow;

pub use geometry::NotchSize;

/// `NSStatusWindowLevel`: above full-screen call windows and normal floating windows.
const STATUS_WINDOW_LEVEL: isize = 25;

define_class!(
    // SAFETY: `NSPanel` has no subclassing requirements and `SottoPanel` adds no ivars, so
    // swapping an `NSWindow` of the same layout to this class is sound.
    #[unsafe(super(NSPanel))]
    #[thread_kind = MainThreadOnly]
    #[name = "SottoPanel"]
    struct SottoPanel;

    impl SottoPanel {
        // A non-activating panel must still become key so the input field can take typing.
        #[unsafe(method(canBecomeKeyWindow))]
        fn can_become_key_window(&self) -> bool {
            true
        }

        #[unsafe(method(canBecomeMainWindow))]
        fn can_become_main_window(&self) -> bool {
            false
        }

        // AppKit would otherwise push the frame below the menu bar; the panel must touch the
        // top of the screen to merge with the hardware notch.
        #[unsafe(method(constrainFrameRect:toScreen:))]
        fn constrain_frame_rect(&self, rect: NSRect, _screen: Option<&NSScreen>) -> NSRect {
            rect
        }
    }
);

fn main_thread() -> Result<MainThreadMarker, String> {
    MainThreadMarker::new().ok_or_else(|| "AppKit calls must run on the main thread".to_string())
}

fn ns_window(window: &WebviewWindow) -> Result<&NSWindow, String> {
    let ptr = window.ns_window().map_err(|e| e.to_string())?;
    if ptr.is_null() {
        return Err("window has no NSWindow".into());
    }
    // SAFETY: `ns_window()` returns the live `NSWindow*` owned by the Tauri window, which
    // outlives the `&WebviewWindow` borrow this reference is tied to. We hold a main-thread
    // marker in every caller, so the object is not used from another thread.
    Ok(unsafe { &*ptr.cast::<NSWindow>() })
}

/// Turns the window into a non-activating panel visible on all Spaces and hidden from Mission
/// Control.
pub fn make_panel(window: &WebviewWindow) -> Result<(), String> {
    let _mtm = main_thread()?;
    let ns = ns_window(window)?;
    let class: &AnyClass = SottoPanel::class();
    // SAFETY: `ns` is a valid window object; `SottoPanel` is a subclass of `NSPanel` with the same
    // instance layout as the window's current class family, as `tauri-nspanel` also relies on.
    unsafe {
        objc2::ffi::object_setClass(
            std::ptr::from_ref::<NSWindow>(ns)
                .cast::<AnyObject>()
                .cast_mut()
                .cast(),
            std::ptr::from_ref::<AnyClass>(class).cast(),
        );
    }
    // SAFETY: the object now has the class `SottoPanel`, so it responds as an `NSPanel`.
    let panel: &NSPanel = unsafe { &*std::ptr::from_ref::<NSWindow>(ns).cast::<NSPanel>() };
    ns.setStyleMask(ns.styleMask() | NSWindowStyleMask::NonactivatingPanel);
    panel.setFloatingPanel(true);
    panel.setBecomesKeyOnlyIfNeeded(true);
    ns.setHidesOnDeactivate(false);
    ns.setLevel(STATUS_WINDOW_LEVEL);
    ns.setCollectionBehavior(
        NSWindowCollectionBehavior::CanJoinAllSpaces
            | NSWindowCollectionBehavior::Stationary
            | NSWindowCollectionBehavior::IgnoresCycle
            | NSWindowCollectionBehavior::FullScreenAuxiliary,
    );
    Ok(())
}

/// Orders the panel to the front and makes it the key window without activating the app, so
/// typing goes to Sotto while the frontmost app keeps its menu bar and focus ring.
pub fn make_key_and_front(window: &WebviewWindow) -> Result<(), String> {
    let _mtm = main_thread()?;
    let ns = ns_window(window)?;
    ns.makeKeyAndOrderFront(None);
    Ok(())
}

/// Hides the window from screenshots, screen recordings and screen shares when `hidden`.
pub fn set_capture_hidden(window: &WebviewWindow, hidden: bool) -> Result<(), String> {
    let _mtm = main_thread()?;
    let ns = ns_window(window)?;
    ns.setSharingType(if hidden {
        NSWindowSharingType::None
    } else {
        NSWindowSharingType::ReadOnly
    });
    Ok(())
}

/// The screen with the menu bar (and the notch), which is where the panel lives.
fn primary_screen(mtm: MainThreadMarker) -> Option<objc2::rc::Retained<NSScreen>> {
    NSScreen::screens(mtm).firstObject()
}

/// Sets the panel frame in one call: top edge on the top of the screen, centered horizontally.
pub fn place_top_centered(window: &WebviewWindow, width: f64, height: f64) -> Result<(), String> {
    let mtm = main_thread()?;
    let ns = ns_window(window)?;
    let screen = primary_screen(mtm).ok_or("no screen")?;
    let frame = screen.frame();
    let origin = NSPoint::new(
        frame.origin.x + (frame.size.width - width) / 2.0,
        frame.origin.y + frame.size.height - height,
    );
    ns.setFrame_display(NSRect::new(origin, NSSize::new(width, height)), true);
    Ok(())
}

/// Size of the hardware notch on the primary screen, if there is one.
pub fn notch_size() -> Option<NotchSize> {
    let mtm = MainThreadMarker::new()?;
    let screen = primary_screen(mtm)?;
    let frame = screen.frame();
    let left = screen.auxiliaryTopLeftArea();
    let right = screen.auxiliaryTopRightArea();
    let top_inset = screen.safeAreaInsets().top;
    geometry::notch_size(
        frame.size.width,
        left.size.width,
        right.size.width,
        top_inset,
    )
}

/// Opens a URL with its default handler (`NSWorkspace`). Only the fixed System Settings URLs in
/// `privacy.rs` are passed here, never text from the webview.
pub fn open_url(url: &str) -> Result<(), String> {
    let _mtm = main_thread()?;
    let url = NSURL::URLWithString(&NSString::from_str(url)).ok_or("not a valid URL")?;
    if NSWorkspace::sharedWorkspace().openURL(&url) {
        Ok(())
    } else {
        Err("macOS did not open the URL".into())
    }
}
