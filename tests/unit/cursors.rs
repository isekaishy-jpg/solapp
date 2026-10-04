use super::*;

fn image() -> SAPreparedCursor {
    SAPreparedCursor {
        rgba: vec![200, 100, 50, 128, 1, 2, 3, 0],
        width: 2,
        height: 1,
        hotspot_x: 1,
        hotspot_y: 0,
    }
}

#[test]
fn prepared_pixels_are_validated_without_transform_or_consuming_original() {
    let input = image();
    let original = input.clone();
    assert!(input.source().is_ok());
    assert_eq!(input, original);
}

#[test]
fn invalid_dimensions_bytes_and_hotspots_are_rejected_before_native_creation() {
    for invalid in [
        SAPreparedCursor {
            width: 0,
            ..image()
        },
        SAPreparedCursor {
            height: 0,
            ..image()
        },
        SAPreparedCursor {
            rgba: vec![0; 7],
            ..image()
        },
        SAPreparedCursor {
            rgba: vec![0; 9],
            ..image()
        },
        SAPreparedCursor {
            hotspot_x: 2,
            ..image()
        },
        SAPreparedCursor {
            hotspot_y: 1,
            ..image()
        },
        SAPreparedCursor {
            width: u16::MAX,
            height: u16::MAX,
            ..image()
        },
    ] {
        let original = invalid.clone();
        assert!(matches!(invalid.source(), Err(SAError::InvalidInput(_))));
        assert_eq!(invalid, original);
    }
}
