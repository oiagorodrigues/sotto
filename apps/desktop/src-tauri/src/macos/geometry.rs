/// Hardware notch dimensions in points.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NotchSize {
    pub width: f64,
    pub height: f64,
}

/// `NSScreen` reports the menu-bar areas on each side of the notch
/// (`auxiliaryTopLeftArea`, `auxiliaryTopRightArea`) and the top safe-area inset; the notch is
/// what is left between them. Screens without a notch report a zero inset.
pub fn notch_size(
    screen_width: f64,
    aux_left_width: f64,
    aux_right_width: f64,
    top_inset: f64,
) -> Option<NotchSize> {
    if top_inset <= 0.0 || aux_left_width <= 0.0 || aux_right_width <= 0.0 {
        return None;
    }
    let width = screen_width - aux_left_width - aux_right_width;
    (width > 0.0).then_some(NotchSize {
        width,
        height: top_inset,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn measures_the_gap_between_the_menu_bar_areas() {
        let n = notch_size(1728.0, 764.0, 764.0, 38.0);
        assert_eq!(
            n,
            Some(NotchSize {
                width: 200.0,
                height: 38.0
            })
        );
    }

    #[test]
    fn no_inset_means_no_notch() {
        assert_eq!(notch_size(1920.0, 960.0, 960.0, 0.0), None);
        assert_eq!(notch_size(1920.0, 0.0, 0.0, 24.0), None);
    }
}
