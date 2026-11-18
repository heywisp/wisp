use super::*;

fn size(width: u32, height: u32, tokens: u64) -> ImageSize {
    ImageSize {
        width,
        height,
        tokens,
    }
}

#[test]
fn the_vision_docs_table_matches_on_both_tiers() {
    let rows = [
        ((200, 200), size(200, 200, 64), size(200, 200, 64)),
        ((1000, 1000), size(1000, 1000, 1296), size(1000, 1000, 1296)),
        ((1092, 1092), size(1092, 1092, 1521), size(1092, 1092, 1521)),
        ((1920, 1080), size(1456, 819, 1560), size(1920, 1080, 2691)),
        ((2000, 1500), size(1270, 952, 1564), size(2000, 1500, 3888)),
        ((3840, 2160), size(1456, 819, 1560), size(2576, 1449, 4784)),
    ];
    for ((w, h), standard, high) in rows {
        assert_eq!(api_size(w, h, Tier::STANDARD), standard, "{w}x{h} standard");
        assert_eq!(api_size(w, h, Tier::HIGH), high, "{w}x{h} high");
    }
}

#[test]
fn the_reference_implementations_examples_match() {
    assert_eq!(api_size(1075, 1520, Tier::STANDARD), size(924, 1307, 1551));
    assert_eq!(api_size(1075, 1520, Tier::HIGH), size(1075, 1520, 2145));
    assert_eq!(api_size(1080, 1920, Tier::STANDARD), size(819, 1456, 1560));
    assert_eq!(api_size(800, 800, Tier::STANDARD), size(800, 800, 841));
}

#[test]
fn the_edge_cap_binds_for_long_thin_images() {
    assert_eq!(api_size(10000, 10, Tier::STANDARD), size(1568, 2, 56));
    assert_eq!(api_size(10, 10000, Tier::HIGH), size(3, 2576, 92));
    assert_eq!(api_size(5000, 3, Tier::STANDARD), size(1568, 1, 56));
    assert_eq!(
        api_size(16384, 16384, Tier::STANDARD),
        size(1092, 1092, 1521)
    );
    assert_eq!(api_size(16384, 16384, Tier::HIGH), size(1932, 1932, 4761));
}

#[test]
fn both_caps_hold_and_nothing_is_ever_enlarged() {
    for tier in [Tier::STANDARD, Tier::HIGH] {
        for w in (1..9000).step_by(397) {
            for h in (1..9000).step_by(331) {
                let s = api_size(w, h, tier);
                assert!(s.width <= w && s.height <= h, "{w}x{h} grew to {s:?}");
                assert!(s.width >= 1 && s.height >= 1, "{w}x{h}: {s:?}");
                assert!(s.tokens <= u64::from(tier.max_tokens), "{w}x{h}: {s:?}");
                assert!(s.width.max(s.height) <= tier.max_edge, "{w}x{h}: {s:?}");
                assert_eq!(s.tokens, tokens(s.width, s.height));
                assert_eq!(api_size(s.width, s.height, tier), s, "{w}x{h} not stable");
            }
        }
    }
}

#[test]
fn the_target_caps_the_long_edge_at_2000_px() {
    assert_eq!(target(3840, 2160, Tier::HIGH), size(2000, 1125, 72 * 41));
    assert_eq!(target(2160, 3840, Tier::HIGH), size(1125, 2000, 72 * 41));
    assert_eq!(target(2576, 10, Tier::HIGH), size(2000, 8, 72));
    assert_eq!(target(1920, 1080, Tier::HIGH), size(1920, 1080, 2691));
    assert_eq!(target(2000, 1500, Tier::HIGH), size(2000, 1500, 3888));
    assert_eq!(target(3840, 2160, Tier::STANDARD), size(1456, 819, 1560));
    for tier in [Tier::STANDARD, Tier::HIGH] {
        for w in (1..16384).step_by(613) {
            for h in (1..16384).step_by(587) {
                let t = target(w, h, tier);
                assert!(t.width.max(t.height) <= MAX_LONG_EDGE, "{w}x{h}: {t:?}");
                assert!(t.width <= w && t.height <= h, "{w}x{h}: {t:?}");
                assert_eq!(api_size(t.width, t.height, tier), t, "{w}x{h}: resent");
            }
        }
    }
}

#[test]
fn degenerate_and_extreme_sizes_never_panic() {
    assert_eq!(api_size(0, 0, Tier::STANDARD), size(0, 0, 0));
    assert_eq!(api_size(0, 500, Tier::HIGH), size(0, 500, 0));
    assert_eq!(target(500, 0, Tier::HIGH), size(500, 0, 0));
    let huge = api_size(u32::MAX, u32::MAX, Tier::STANDARD);
    assert_eq!(huge, size(1092, 1092, 1521));
    let thin = api_size(u32::MAX, 1, Tier::HIGH);
    assert_eq!(thin, size(2576, 1, 92));
    assert_eq!(target(u32::MAX, 1, Tier::HIGH), size(2000, 1, 72));
    assert_eq!(tokens(u32::MAX, u32::MAX), 153_391_690 * 153_391_690);
}

#[test]
fn a_custom_tier_is_honoured() {
    let tiny = Tier {
        max_edge: 56,
        max_tokens: 2,
    };
    assert_eq!(api_size(100, 50, tiny), size(56, 28, 2));
}

#[test]
fn capping_keeps_the_aspect_and_never_enlarges() {
    assert_eq!(capped(4000, 2000, 1000), size(1000, 500, 36 * 18));
    assert_eq!(capped(2000, 4000, 1000), size(500, 1000, 18 * 36));
    assert_eq!(capped(300, 200, 1000), size(300, 200, 11 * 8));
    assert_eq!(capped(0, 7, 1), size(0, 7, 0));
}
