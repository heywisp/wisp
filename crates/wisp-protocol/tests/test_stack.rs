use std::hint::black_box;

const FRAME_BYTES: usize = 64 * 1024;
const FRAMES: usize = 96;

fn descend(depth: usize) -> usize {
    let mut frame = [u8::try_from(depth % 256).unwrap_or_default(); FRAME_BYTES];
    black_box(&mut frame);
    let below = if depth == 0 { 0 } else { descend(depth - 1) };
    black_box(below + usize::from(frame[depth % FRAME_BYTES]))
}

#[test]
fn a_test_thread_has_room_for_six_mib_of_frames() {
    let expected: usize = (0..FRAMES).map(|depth| depth % 256).sum();
    assert_eq!(descend(black_box(FRAMES - 1)), expected);
}
