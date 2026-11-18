use std::time::Duration;

use wisp_protocol::agents::ROOT_NAME;
use wisp_protocol::slot::{NodeState, SlotTopologyNode};

use super::*;
use crate::canvas::Mark as Cell;
use crate::motion::MotionPolicy;
use crate::picture::Mark;
use crate::picture::{Rect, tiles};
use crate::testing::trimmed;
use crate::theme::glyphs::GlyphSet;
use crate::theme::{Depth, Mode, Theme};

const CHROME_ROWS: usize = 11;
const STILL: MotionCtx = MotionCtx {
    policy: MotionPolicy::Reduced,
    ascii: false,
    blend: false,
};
const NOBODY: &str = "nobody";

const SIX: &str = "agent-viz=w agent-viz/planner=d agent-viz/plan-review-a=d agent-viz/plan-review-b=s agent-viz/coder=w agent-viz/helper=d";

const TWENTY: &str = "helper=d agent-viz=o agent-viz/planner=w agent-viz/plan-review-a=d agent-viz/plan-review-b=w resume-list-speed=w resume-list-speed/planner=d resume-list-speed/coder=w resume-list-speed/code-review=o resume-list-speed/helper=w cache-fingerprints=o cache-fingerprints/coder=d cache-fingerprints/code-review=f cache-fingerprints/helper=s docs-batch=w docs-batch/coder=w docs-batch/code-review=o docs-batch/helper=d merge=w";

const TALL: &str = "agent-viz=w agent-viz/code-review=o agent-viz/coder=w agent-viz/coder/helper=w agent-viz/coder/helper/reader=w agent-viz/coder/helper/reader/fetcher=w agent-viz/planner=d";

const CROWD: &str = "agent-viz=w agent-viz/code-review=w agent-viz/coder=o agent-viz/helper=d cache-fingerprints=o cache-fingerprints/code-review=w cache-fingerprints/coder=s cache-fingerprints/helper=w config-panel=w config-panel/code-review=f config-panel/coder=d config-panel/helper=w docs-batch=w docs-batch/code-review=o docs-batch/coder=d docs-batch/helper=w plugin-wit=o plugin-wit/code-review=o plugin-wit/coder=d plugin-wit/helper=w resume-list-speed=w resume-list-speed/code-review=s resume-list-speed/coder=w resume-list-speed/helper=f sse-reconnect=d sse-reconnect/code-review=d sse-reconnect/coder=w sse-reconnect/helper=o voice-input=w voice-input/code-review=d voice-input/coder=w voice-input/helper=o helper=d merge=w";

const FLAT: &str = "coder=w docs-writer=d fixer=w helper=d researcher=o reviewer=f auditor=s benchmark=w linter=d planner=o tester=w translator=d";

const SIX_120: &str = r#"






                                                            ⠶
                                                            ⡇
                                                            ⡇
                                                            ⡇
                                                            ⡇
                                                            ⡇
                                                            ⡇
                                                            ●
                                                            ⡇
                                                            ⡇
            ⡖⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⡖⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⡗⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⡖⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⡆
            ⡇                       ⡇                       ⡇                       ⡇                       ⡇
            ⡇                       ⡇                       ⡇                       ⡇                       ⡇
            ⡇                       ⡇                       ⡇                       ⡇                       ⡇
           [●]                      ✓                       ✓                       ■                       ✓







 ⠶ main  ● working  ○ waiting  ✓ done  ■ stopped  ⨯ failed
"#;

const SIX_80: &str = r#"
                                        ⠶
                                        ⡇
                                        ⡇
                                        ⡇
                                        ⡇
                                        ●
                                        ⡇
        ⡖⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⡖⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⡗⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⡖⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⡆
        ⡇               ⡇               ⡇               ⡇               ⡇
        ⡇               ⡇               ⡇               ⡇               ⡇
       [●]              ✓               ✓               ■               ✓

 ⠶ main  ● working  ○ waiting  ✓ done  ■ stopped  ⨯ failed
"#;

const SIX_40: &str = r#"

                    ⠶
                    ⡇
                    ⡇
                    ⡇
                    ⡇
                    ⡇
                    ⡇
                    ●
                    ⡇
                    ⡇
    ⡖⠒⠒⠒⠒⠒⠒⠒⡖⠒⠒⠒⠒⠒⠒⠒⡗⠒⠒⠒⠒⠒⠒⠒⡖⠒⠒⠒⠒⠒⠒⠒⡆
    ⡇       ⡇       ⡇       ⡇       ⡇
    ⡇       ⡇       ⡇       ⡇       ⡇
    ⡇       ⡇       ⡇       ⡇       ⡇
   [●]      ✓       ✓       ■       ✓

 ⠶ main  ● working  ○ waiting  ✓ done
 ■ stopped  ⨯ failed
"#;

const TWENTY_120: &str = r#"






                                                            ⠶
                                                            ⡇
                                                            ⡇
          ⡖⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⡖⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⡖⠒⠒⠒⠒⠒⠒⠒⠓⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⡖⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⡖⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⡆
          ⡇                    ⡇                    ⡇                  ⡇                ⡇                   ⡇
          ⡇                    ⡇                    ⡇                  ⡇                ⡇                   ⡇
          ⡇                    ⡇                    ⡇                  ⡇                ⡇                   ⡇
          ○                    ○                    ●                  ✓                ●                   ●
          ⡇                    ⡇                    ⡇                                                       ⡇
          ⡇                    ⡇                    ⡇                                                       ⡇
   ⡖⠒⠒⠒⠒⠒⠒⡗⠒⠒⠒⠒⠒⠒⡆      ⡖⠒⠒⠒⠒⠒⠒⡗⠒⠒⠒⠒⠒⠒⡆      ⡖⠒⠒⠒⠒⠒⠒⡗⠒⠒⠒⠒⠒⠒⡆                                       ⡖⠒⠒⠒⠒⠒⡖⠒⠒⠓⠒⠒⡖⠒⠒⠒⠒⠒⡆
   ⡇      ⡇      ⡇      ⡇      ⡇      ⡇      ⡇      ⡇      ⡇                                       ⡇     ⡇     ⡇     ⡇
   ⡇      ⡇      ⡇      ⡇      ⡇      ⡇      ⡇      ⡇      ⡇                                       ⡇     ⡇     ⡇     ⡇
   ⡇      ⡇      ⡇      ⡇      ⡇      ⡇      ⡇      ⡇      ⡇                                       ⡇     ⡇     ⡇     ⡇
   ✓      ●      ●      ⨯      ✓      ■      ○      ●      ✓                                       ○    [●]    ●     ✓







 ⠶ main  ● working  ○ waiting  ✓ done  ■ stopped  ⨯ failed
"#;

const TWENTY_80: &str = r#"
                                        ⠶
                                        ⡇
       ⡖⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⡖⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⡖⠒⠒⠒⠒⠓⠒⠒⠒⠒⠒⠒⠒⡖⠒⠒⠒⠒⠒⠒⠒⠒⠒⡖⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⡆
       ⡇             ⡇             ⡇            ⡇         ⡇            ⡇
       ⡇             ⡇             ⡇            ⡇         ⡇            ⡇
       ○             ○             ●            ✓         ●            ●
       ⡇             ⡇             ⡇                                   ⡇
  ⡖⠒⠒⠒⠒⡗⠒⠒⠒⡆    ⡖⠒⠒⠒⠒⡗⠒⠒⠒⠒⡆    ⡖⠒⠒⠒⡗⠒⠒⠒⠒⡆                        ⡖⠒⠒⠒⡖⠒⠓⠒⡖⠒⠒⠒⡆
  ⡇    ⡇   ⡇    ⡇    ⡇    ⡇    ⡇   ⡇    ⡇                        ⡇   ⡇   ⡇   ⡇
  ⡇    ⡇   ⡇    ⡇    ⡇    ⡇    ⡇   ⡇    ⡇                        ⡇   ⡇   ⡇   ⡇
  ✓    ●   ●    ⨯    ✓    ■    ○   ●    ✓                        ○  [●]  ●   ✓

 ⠶ main  ● working  ○ waiting  ✓ done  ■ stopped  ⨯ failed
"#;

const TWENTY_40: &str = r#"

                    ⠶
                    ⡇
                    ⡇
   ⡖⠒⠒⠒⠒⠒⠒⠒⡖⠒⠒⠒⠒⠒⠒⠒⡖⠓⠒⠒⠒⡖⠒⠒⠒⡖⠒⠒⠒⠒⠒⠒⡆
   ⡇       ⡇       ⡇    ⡇   ⡇      ⡇
   ⡇       ⡇       ⡇    ⡇   ⡇      ⡇
   ⡇       ⡇       ⡇    ⡇   ⡇      ⡇
   ○       ○       ●    ✓   ●      ●
   ⡇       ⡇       ⡇               ⡇
   ⡇       ⡇       ⡇               ⡇
 ⡖⠒⡗⠒⠒⡆ ⡖⠒⠒⡗⠒⠒⡆ ⡖⠒⠒⡗⠒⡆         ⡖⠒⡖⠒⠓⡖⠒⡆
 ⡇ ⡇  ⡇ ⡇  ⡇  ⡇ ⡇  ⡇ ⡇         ⡇ ⡇  ⡇ ⡇
 ⡇ ⡇  ⡇ ⡇  ⡇  ⡇ ⡇  ⡇ ⡇         ⡇ ⡇  ⡇ ⡇
 ⡇ ⡇  ⡇ ⡇  ⡇  ⡇ ⡇  ⡇ ⡇         ⡇ ⡇  ⡇ ⡇
 ✓ ●  ● ⨯  ✓  ■ ○  ● ✓         ○[●] ● ✓

 ⠶ main  ● working  ○ waiting  ✓ done
 ■ stopped  ⨯ failed
"#;

const FLAT_80: &str = r#"


                                        ⠶
                                        ⡇
                                        ⡇
   ⡖⠒⠒⠒⠒⠒⠒⡖⠒⠒⠒⠒⠒⡖⠒⠒⠒⠒⠒⠒⡖⠒⠒⠒⠒⠒⠒⡖⠒⠒⠒⠒⠒⡖⠒⠒⠒⠓⠒⠒⡖⠒⠒⠒⠒⠒⠒⡖⠒⠒⠒⠒⠒⡖⠒⠒⠒⠒⠒⠒⡖⠒⠒⠒⠒⠒⡖⠒⠒⠒⠒⠒⠒⡆
   ⡇      ⡇     ⡇      ⡇      ⡇     ⡇      ⡇      ⡇     ⡇      ⡇     ⡇      ⡇
   ⡇      ⡇     ⡇      ⡇      ⡇     ⡇      ⡇      ⡇     ⡇      ⡇     ⡇      ⡇
   ⡇      ⡇     ⡇      ⡇      ⡇     ⡇      ⡇      ⡇     ⡇      ⡇     ⡇      ⡇
   ■      ●    [●]     ✓      ●     ✓      ✓      ○     ○      ⨯     ●      ✓


 ⠶ main  ● working  ○ waiting  ✓ done  ■ stopped  ⨯ failed
"#;

const FLAT_40: &str = r#"




                    ⠶
                    ⡇
                    ⡇
 ⡖⠒⠒⠒⡖⠒⠒⡖⠒⠒⡖⠒⠒⡖⠒⠒⠒⡖⠒⠓⡖⠒⠒⡖⠒⠒⠒⡖⠒⠒⡖⠒⠒⡖⠒⠒⠒⡆
 ⡇   ⡇  ⡇  ⡇  ⡇   ⡇  ⡇  ⡇   ⡇  ⡇  ⡇   ⡇
 ⡇   ⡇  ⡇  ⡇  ⡇   ⡇  ⡇  ⡇   ⡇  ⡇  ⡇   ⡇
 ⡇   ⡇  ⡇  ⡇  ⡇   ⡇  ⡇  ⡇   ⡇  ⡇  ⡇   ⡇
 ■   ● [●] ✓  ●   ✓  ✓  ○   ○  ⨯  ●   ✓





 ⠶ main  ● working  ○ waiting  ✓ done
 ■ stopped  ⨯ failed
"#;

const TALL_80: &str = r#"
▴                                       ●
             ⡖⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⡗⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⡆
             ⡇                          ⡇                         ⡇
             ○                          ●                         ✓
                                        ⡇
                                        ⡇
                                       [●]
                                        ⡇
                                        ⡇
                                        ●
                                        ⡇
▾                                       ⡇
 ⠶ main  ● working  ○ waiting  ✓ done  ■ stopped  ⨯ failed
"#;

const CROWD_120: &str = r#"






                                                            ⠶
                                                            ⡇
                                                            ⡇
      ⡖⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⡖⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⡖⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⡖⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⡖⠒⠒⠒⠒⠓⠒⠒⠒⡖⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⡖⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⡖⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⡖⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⡆
      ⡇            ⡇            ⡇           ⡇          ⡇        ⡇          ⡇            ⡇           ⡇            ⡇
      ⡇            ⡇            ⡇           ⡇          ⡇        ⡇          ⡇            ⡇           ⡇            ⡇
      ⡇            ⡇            ⡇           ⡇          ⡇        ⡇          ⡇            ⡇           ⡇            ⡇
      ●            ○            ●           ●          ✓        ●          ○            ●           ✓            ●
      ⡇            ⡇            ⡇           ⡇                              ⡇            ⡇           ⡇            ⡇
      ⡇            ⡇            ⡇           ⡇                              ⡇            ⡇           ⡇            ⡇
  ⡖⠒⠒⠒⡗⠒⠒⠒⡆   ⡖⠒⠒⠒⠒⡗⠒⠒⠒⡆   ⡖⠒⠒⠒⠒⡗⠒⠒⠒⡆   ⡖⠒⠒⠒⡗⠒⠒⠒⠒⡆                    ⡖⠒⠒⠒⠒⡗⠒⠒⠒⡆   ⡖⠒⠒⠒⠒⡗⠒⠒⠒⡆   ⡖⠒⠒⠒⡗⠒⠒⠒⠒⡆   ⡖⠒⠒⠒⡗⠒⠒⠒⡆
  ⡇   ⡇   ⡇   ⡇    ⡇   ⡇   ⡇    ⡇   ⡇   ⡇   ⡇    ⡇                    ⡇    ⡇   ⡇   ⡇    ⡇   ⡇   ⡇   ⡇    ⡇   ⡇   ⡇   ⡇
  ⡇   ⡇   ⡇   ⡇    ⡇   ⡇   ⡇    ⡇   ⡇   ⡇   ⡇    ⡇                    ⡇    ⡇   ⡇   ⡇    ⡇   ⡇   ⡇   ⡇    ⡇   ⡇   ⡇   ⡇
  ⡇   ⡇   ⡇   ⡇    ⡇   ⡇   ⡇    ⡇   ⡇   ⡇   ⡇    ⡇                    ⡇    ⡇   ⡇   ⡇    ⡇   ⡇   ⡇   ⡇    ⡇   ⡇   ⡇   ⡇
  ●  [○]  ✓   ●    ■   ●   ⨯    ✓   ●   ○   ✓    ●                    ○    ✓   ●   ■    ●   ⨯   ✓   ●    ○   ✓   ●   ○







 ⠶ main  ● working  ○ waiting  ✓ done  ■ stopped  ⨯ failed
"#;

const SIX_120_ASCII: &str = r#"






                                                            *
                                                            |
                                                            |
                                                            |
                                                            |
                                                            |
                                                            |
                                                            @
                                                            |
                                                            |
            .-----------------------.-----------------------|-----------------------.-----------------------.
            |                       |                       |                       |                       |
            |                       |                       |                       |                       |
            |                       |                       |                       |                       |
           [@]                      +                       +                       =                       +







 * main  @ working  o waiting  + done  = stopped  x failed
"#;

const BOX: &str = r#"
                 ⠶
  ⡖⠒⠒⠒⡖⠒⠒⠒⠒⠒⠒⠒⠒⡖⠒⠓⠒⠒⠒⠒⠒⡖⠒⠒⠒⠒⠒⠒⠒⡆
  ⡇   ⡇        ⡇       ⡇       ⡇
  ●   ○        ●       ✓+30    ○
           ⡖⠒⠒⠒⡗⠒⡖⠒⡆          ⡖⠓⠒⡆
           ⡇   ⡇ ⡇ ⡇          ⡇  ⡇
           ●   ✓ ● ○          ○  ✓
           ⡇
           ⡇
           ●
          ⡖⠓⡆
          ⡇ ⡇
          ● ✓

⠶ main  ● working  ○ waiting
✓ done  ■ stopped  ⨯ failed
"#;

const BOX_ASCII: &str = r#"
                 *
  .---.--------.-'-----.-------.
  |   |        |       |       |
  @   o        @       ++30    o
           .---|-.-.          .'-.
           |   | | |          |  |
           @   + @ o          o  +
           |
           |
           @
          .'.
          | |
          @ +

* main  @ working  o waiting
+ done  = stopped  x failed
"#;

const BOX_40: &str = r#"
                  ⠶
  ⡖⠒⠒⠒⡖⠒⠒⠒⠒⠒⠒⠒⠒⡖⠒⠒⠓⠒⠒⠒⠒⠒⡖⠒⠒⠒⠒⠒⠒⠒⡆
  ⡇   ⡇        ⡇        ⡇       ⡇
  ●   ○        ●        ✓+30    ○
           ⡖⠒⠒⠒⡗⠒⠒⡖⠒⡆          ⡖⠓⠒⡆
           ⡇   ⡇  ⡇ ⡇          ⡇  ⡇
           ●   ✓  ● ○          ○  ✓
           ⡇
           ⡇
           ●
          ⡖⠓⡆
          ⡇ ⡇
          ● ✓

⠶ main  ● working  ○ waiting
✓ done  ■ stopped  ⨯ failed
"#;

const BOX_C_FAILS: &str = r#"
                 ⠶
 ⡖⠒⠒⡖⠒⠒⡖⠒⠒⠒⡖⠒⠒⠒⠒⠒⠓⠒⠒⠒⠒⠒⠒⠒⡆
 ⡇  ⡇  ⡇   ⡇             ⡇
 ●  ○  ●+7 ✓+30          ○
                 ⡖⠒⡖⠒⡖⠒⡖⠒⡗⠒⡖⠒⡖⠒⡖⠒⡆
                 ⡇ ⡇ ⡇ ⡇ ⡇ ⡇ ⡇ ⡇ ⡇
                 ○ ✓ ○ ○ ⨯ ○ ○ ○ ○

⠶ main  ● working  ○ waiting
✓ done  ■ stopped  ⨯ failed
"#;

fn scene(spec: &str) -> Vec<SlotTopologyNode> {
    let items: BTreeMap<&str, &str> = spec
        .split_whitespace()
        .map(|item| item.split_once('=').unwrap())
        .collect();
    items
        .into_iter()
        .map(|(key, state)| SlotTopologyNode {
            key: key.to_owned(),
            parent: key.rsplit_once('/').map(|(parent, _)| parent.to_owned()),
            state: match state {
                "w" => NodeState::Working,
                "o" => NodeState::Waiting,
                "d" => NodeState::Done,
                "s" => NodeState::Stopped,
                _ => NodeState::Failed,
            },
        })
        .collect()
}

fn view<'a>(
    nodes: &'a [SlotTopologyNode],
    picked: &'a str,
    width: usize,
    set: GlyphSet,
) -> Topology<'a> {
    Topology {
        root: Some(ROOT_NAME),
        nodes,
        flows: &[],
        picked: Some(picked),
        label: None,
        layout: Layout::new(width, set),
        at: Duration::ZERO,
        now_ms: 0,
        cx: STILL,
        pictures: false,
    }
}

fn text(lines: &[StyledLine]) -> Vec<String> {
    let mut rows = trimmed(lines);
    while rows.last().is_some_and(String::is_empty) {
        rows.pop();
    }
    rows
}

fn want(drawing: &str) -> Vec<&str> {
    drawing
        .strip_prefix('\n')
        .unwrap_or(drawing)
        .lines()
        .collect()
}

fn paged(spec: &str, picked: &str, (width, height): (usize, usize), set: GlyphSet) -> Vec<String> {
    let nodes = scene(spec);
    text(&page(&view(&nodes, picked, width, set), height - CHROME_ROWS).lines)
}

fn boxed(spec: &str, width: usize, set: GlyphSet) -> Vec<String> {
    let nodes = scene(spec);
    text(&panel(&view(&nodes, NOBODY, width, set)).lines)
}

fn has(rows: &[String], glyph: char) -> bool {
    rows.iter().any(|row| row.contains(glyph))
}

#[test]
fn six_agents_at_120_80_and_40_columns_are_the_frames() {
    for (size, drawing) in [((120, 40), SIX_120), ((80, 24), SIX_80), ((40, 30), SIX_40)] {
        let drawn = paged(SIX, "agent-viz/coder", size, GlyphSet::Unicode);
        assert_eq!(drawn, want(drawing), "{size:?}");
    }
}

#[test]
fn twenty_agents_at_120_80_and_40_columns_are_the_frames() {
    let sizes = [
        ((120, 40), TWENTY_120),
        ((80, 24), TWENTY_80),
        ((40, 30), TWENTY_40),
    ];
    for (size, drawing) in sizes {
        let drawn = paged(TWENTY, "resume-list-speed/coder", size, GlyphSet::Unicode);
        assert_eq!(drawn, want(drawing), "{size:?}");
    }
}

#[test]
fn twelve_agents_all_started_by_the_root_fan_out_from_one_crossbar() {
    for (size, drawing) in [((80, 24), FLAT_80), ((40, 30), FLAT_40)] {
        let drawn = paged(FLAT, "coder", size, GlyphSet::Unicode);
        assert_eq!(drawn, want(drawing), "{size:?}");
    }
}

#[test]
fn a_crowded_lane_widens_and_a_page_too_narrow_scrolls_a_lane_at_a_time_or_to_the_dot() {
    assert_eq!(
        paged(CROWD, "agent-viz/coder", (120, 40), GlyphSet::Unicode),
        want(CROWD_120)
    );
    let narrow_spec = |spec: &str, picked| paged(spec, picked, (40, 30), GlyphSet::Unicode);
    let narrow = |picked| narrow_spec(CROWD, picked);
    for (picked, left, right) in [
        ("agent-viz/coder", false, true),
        ("sse-reconnect/coder", true, true),
        ("voice-input/helper", true, false),
    ] {
        let rows = narrow(picked);
        assert!(rows.iter().all(|r| r.chars().count() <= 40), "{rows:#?}");
        assert!(has(&rows, '[') && has(&rows, ']'), "{picked}: {rows:#?}");
        assert_eq!(has(&rows, '◂'), left, "{picked}: {rows:#?}");
        assert_eq!(has(&rows, '▸'), right, "{picked}: {rows:#?}");
    }
    let wide: String = (0..25).map(|i| format!(" a/c{i:02}=d")).collect();
    let tail: String = (0..30).map(|i| format!(" t{i:02}=d")).collect();
    let mid: String = (0..19).map(|i| format!(" c/k{i:02}=d")).collect();
    let more: String = (0..20).map(|i| format!(" z{i:02}=d")).collect();
    for (spec, picked) in [
        (format!("a=w{wide}"), "a/c24"),
        (format!("a=w{wide}{tail}"), "a/c20"),
        (format!("b=w c=w{mid} d=w{more}"), "c"),
    ] {
        let rows = narrow_spec(&spec, picked);
        let bracketed = rows.iter().any(|r| {
            let cells: Vec<char> = r.chars().collect();
            cells.windows(3).any(|w| w[0] == '[' && w[2] == ']')
        });
        assert!(bracketed, "{picked}: {rows:#?}");
    }
    let ascii = paged(CROWD, "sse-reconnect/coder", (40, 30), GlyphSet::Ascii);
    assert!(
        ascii.iter().any(|r| r.starts_with('<') && r.ends_with('>')),
        "{ascii:#?}"
    );
}

#[test]
fn a_deep_tree_scrolls_up_and_down_to_keep_the_picked_dot_in_view() {
    assert_eq!(
        paged(TALL, "agent-viz/coder/helper", (80, 24), GlyphSet::Unicode),
        want(TALL_80)
    );
    let top = paged(TALL, "agent-viz", (80, 24), GlyphSet::Unicode);
    assert!(!has(&top, '▴') && has(&top, '▾'), "{top:#?}");
    let deepest = "agent-viz/coder/helper/reader/fetcher";
    let bottom = paged(TALL, deepest, (80, 24), GlyphSet::Ascii);
    assert!(bottom[0].starts_with('^'), "{bottom:#?}");
    assert!(bottom.iter().all(|r| !r.starts_with('v')), "{bottom:#?}");
    assert!(bottom.iter().any(|r| r.trim() == "[@]"), "{bottom:#?}");
    let roomy = paged(TALL, "agent-viz", (120, 40), GlyphSet::Unicode);
    assert!(!has(&roomy, '▴') && !has(&roomy, '▾'), "{roomy:#?}");
}

#[test]
fn the_cells_beside_every_dot_are_blank_so_the_brackets_always_fit() {
    let scenes = [SIX, TWENTY, FLAT, TALL, CROWD];
    for spec in scenes {
        for size in [(120, 40), (80, 24), (40, 30)] {
            let plain = paged(spec, NOBODY, size, GlyphSet::Unicode);
            if plain
                .iter()
                .any(|r| r.starts_with(['▴', '▾', '◂']) || r.ends_with('▸'))
            {
                continue;
            }
            let cells = |rows: &[String]| -> Vec<Vec<char>> {
                rows.iter()
                    .map(|r| {
                        r.chars()
                            .chain(std::iter::repeat(' '))
                            .take(size.0)
                            .collect()
                    })
                    .collect()
            };
            let before = cells(&plain);
            for picked in scene(spec).iter().map(|n| n.key.as_str()).chain([""]) {
                let target = (!picked.is_empty()).then_some(picked);
                let nodes = scene(spec);
                let topology = Topology {
                    picked: target,
                    ..view(&nodes, NOBODY, size.0, GlyphSet::Unicode)
                };
                let after = cells(&text(&page(&topology, size.1 - CHROME_ROWS).lines));
                let changed: Vec<(char, char)> = before
                    .iter()
                    .flatten()
                    .zip(after.iter().flatten())
                    .filter(|(b, a)| b != a)
                    .map(|(b, a)| (*b, *a))
                    .collect();
                assert_eq!(changed, [(' ', '['), (' ', ']')], "{picked} at {size:?}");
            }
        }
    }
}

fn f1(changes: &str) -> String {
    let b: String = (0..30).map(|i| format!(" task-b/c{i:02}=d")).collect();
    format!(
        "helper=w merge=o task-a=w task-a/coder=w task-a/coder/helper=w task-a/coder/helper/a=w task-a/coder/helper/b=d task-a/helper=d task-a/planner=w task-a/review=o task-b=d{b} task-c=o task-c/coder=o task-c/review=d {changes}"
    )
}

fn busy(changes: &str) -> String {
    let c: String = (1..7).map(|i| format!(" task-c/r{i}=o")).collect();
    f1(&format!("task-c/planner=d task-c/review=o{c} {changes}"))
}

#[test]
fn the_box_puts_main_on_the_first_row_and_each_generation_three_rows_down() {
    assert_eq!(boxed(&f1(""), 35, GlyphSet::Unicode), want(BOX));
    assert_eq!(boxed(&f1(""), 36, GlyphSet::Unicode), want(BOX_40));
    assert_eq!(boxed(&f1(""), 35, GlyphSet::Ascii), want(BOX_ASCII));
    for width in [20, 35, 120] {
        let rows = boxed(SIX, width, GlyphSet::Unicode);
        let legend = &rows[rows.len() - 2..];
        assert_eq!(legend[0].matches(['⠶', '●', '○']).count(), 3, "{rows:#?}");
        assert!(legend[1].starts_with("✓ done"), "{rows:#?}");
    }
}

#[test]
fn a_family_wider_than_the_box_folds_the_quietest_first_and_only_while_needed() {
    let looks = |spec: &str, width, row: usize, token| -> String {
        let nodes = scene(spec);
        let lines = panel(&view(&nodes, NOBODY, width, GlyphSet::Unicode)).lines;
        let spans = lines[row]
            .spans
            .iter()
            .filter(|s| s.look == Look::fg(token));
        spans.map(|s| s.text.as_str()).collect()
    };
    assert_eq!(looks(&f1(""), 35, 3, Token::Ok), "✓+30");
    assert_eq!(
        boxed(&f1(""), 35, GlyphSet::Unicode)[3].chars().nth(27),
        Some(' ')
    );
    assert_eq!(boxed(&f1("task-b/c07=f"), 35, GlyphSet::Unicode), want(BOX));
    assert_eq!(looks(&f1("task-b/c07=f"), 35, 3, Token::Error), "+30");
    let before = boxed(&busy(""), 35, GlyphSet::Unicode);
    assert!(
        before[3].contains("✓+30") && before[3].contains("○+9"),
        "{before:#?}"
    );
    let after = boxed(&busy("task-c/r3=f"), 35, GlyphSet::Unicode);
    assert_eq!(after, want(BOX_C_FAILS));
    assert_eq!(before.len() - after.len(), 6);
    let leaves = |f: &str, n: usize, state: &str| -> String {
        (0..n).map(|i| format!(" {f}/{i}={state}")).collect()
    };
    let cases = [
        (3, "d", 12, "   ●    ●+3", 3),
        (4, "d", 14, "  ●+4     ●", 3),
        (2, "w", 10, "  ●   ●+3", 2),
    ];
    for (a, b, width, folded, open) in cases {
        let spec = format!("a=w{} b=w{}", leaves("a", a, "d"), leaves("b", 3, b));
        let rows = boxed(&spec, width, GlyphSet::Unicode);
        assert_eq!(rows[3], folded, "{rows:#?}");
        assert_eq!(rows[6].matches(['✓', '●']).count(), open, "{rows:#?}");
    }
    let nested = format!(
        "a=d a/x=d{} a/x/5=f b=w{}",
        leaves("a/x", 5, "d"),
        leaves("b", 6, "w")
    );
    assert!(boxed(&nested, 14, GlyphSet::Unicode)[6].contains("✓+6"));
    assert_eq!(looks(&nested, 14, 6, Token::Error), "+6");
    let root = boxed(SIX, 4, GlyphSet::Unicode);
    assert_eq!(root[..2], [" ⠶+6", ""]);
}

#[test]
fn ascii_draws_the_lines_with_bars_dashes_dots_and_quotes() {
    assert_eq!(
        paged(SIX, "agent-viz/coder", (120, 40), GlyphSet::Ascii),
        want(SIX_120_ASCII)
    );
    let nodes = scene(SIX);
    for (set, braille) in [(GlyphSet::Unicode, true), (GlyphSet::Ascii, false)] {
        let tree = Tree::new(&nodes);
        let placed = Placed::page(&tree, true, 40, 0);
        let view = view(&nodes, NOBODY, 40, set);
        let mut drawn = draw(&placed, &view, &flights(&view, &tree, &placed), None);
        let wires: Vec<Cell> = (0..placed.height)
            .flat_map(|y| (0..placed.width).map(move |x| (x, y)))
            .filter_map(|at| drawn.cell_mut(at).copied().flatten())
            .filter(|cell| cell.look == wire())
            .map(|cell| cell.mark)
            .collect();
        assert!(!wires.is_empty(), "{set:?}");
        assert!(
            wires
                .iter()
                .all(|mark| matches!(mark, Cell::Bits { .. }) == braille),
            "{set:?}"
        );
    }
}

#[test]
fn the_legend_names_every_known_state_once() {
    let legend = |state| match state {
        NodeState::Working
        | NodeState::Waiting
        | NodeState::Done
        | NodeState::Stopped
        | NodeState::Failed => true,
        NodeState::Unknown => false,
    };
    use NodeState::*;
    let every = [Working, Waiting, Done, Stopped, Failed, Unknown];
    let mut listed = STATES.to_vec();
    listed.sort();
    listed.dedup();
    assert_eq!(listed.len(), STATES.len(), "{STATES:?}");
    assert_eq!(every.iter().filter(|s| legend(**s)).count(), STATES.len());
    assert!(STATES.iter().all(|s| legend(*s)), "{STATES:?}");
}

#[test]
fn a_breathing_step_changes_only_the_working_dots_own_tiles() {
    let nodes = scene("a=w b=d a/c=w a/d=o");
    let at = |ms| {
        let view = Topology {
            pictures: true,
            at: Duration::from_millis(ms),
            cx: MotionCtx::new(MotionPolicy::Full, GlyphSet::Unicode, Depth::TrueColor),
            ..view(&nodes, NOBODY, 30, GlyphSet::Unicode)
        };
        panel(&view).scenes.remove(0)
    };
    let cell = (9, 19);
    let (before, after) = (at(0), at(600));
    assert_eq!(tiles(&at(0), cell), tiles(&at(150), cell), "a 200 ms grid");
    let working: Vec<(usize, usize)> = before
        .dots
        .iter()
        .filter(|d| d.halo && d.mark == Mark::State(NodeState::Working))
        .map(|d| (d.at.0 as usize, d.at.1 as usize))
        .collect();
    assert_eq!(working.len(), 2);
    let changed: Vec<Rect> = tiles(&before, cell)
        .into_iter()
        .zip(tiles(&after, cell))
        .filter(|(a, b)| a != b)
        .map(|(a, _)| a.0)
        .collect();
    let under = |r: &Rect| {
        working
            .iter()
            .any(|&(x, y)| y == r.y && (r.x..r.x + r.cells).contains(&x))
    };
    assert_eq!(changed.len(), working.len(), "{changed:?} {working:?}");
    assert!(changed.iter().all(under), "{changed:?} against {working:?}");
}

#[test]
fn a_hovered_dot_s_name_sits_under_it_inside_the_picture() {
    let nodes = scene("a=w b=w c=w d=w");
    let view = view(&nodes, NOBODY, 32, GlyphSet::Unicode);
    let look = Look::fg(Token::Text).on(Token::AccentTint).bold();
    let height = 18;
    let glyphs = Glyphs::of(view.layout.glyphs);
    let legend = legend(&view, &glyphs).0.len();
    let positions = super::paged(&view, height, legend).spots;
    let draw = |key, name, height| {
        page(
            &Topology {
                label: Some((key, name)),
                ..view
            },
            height,
        )
    };
    let band = |drawing: &Drawing, y: usize| {
        drawing.lines[y]
            .spans
            .iter()
            .find(|s| s.look == look)
            .unwrap()
            .text
            .clone()
    };
    let (root_x, root_y) = positions[0].1;
    let main = draw(None, ROOT_NAME, height);
    assert_eq!(band(&main, root_y + 1), " main ");
    assert_eq!(look_at(&main.lines, (root_x - 3, root_y + 1)), Some(look));
    let name = "a-name-that-reaches-the-edge";
    let band_width = name.len() + 2;
    for (key, edge) in [(positions[1].0, 0), (positions[4].0, 32 - band_width)] {
        let y = positions[1].1.1;
        let over = draw(key, name, legend + y + 1);
        assert_eq!(band(&over, y - 1), format!(" {name} "));
        assert_eq!(look_at(&over.lines, (edge, y - 1)), Some(look));
        assert_eq!(
            look_at(&over.lines, (edge + band_width - 1, y - 1)),
            Some(look)
        );
    }
    let too_long = "long-name-".repeat(10);
    let y = positions[1].1.1 + 1;
    let fitted = band(&draw(Some("a"), &too_long, height), y);
    assert_eq!(width(&fitted, view.layout.width_mode), 32);
    assert!(fitted.ends_with("… "), "{fitted:?}");

    let painted = |label| {
        page(
            &Topology {
                pictures: true,
                layout: Layout {
                    width: 100,
                    ..view.layout
                },
                label,
                ..view
            },
            height,
        )
        .scenes
        .remove(0)
    };
    let cell = (9, 19);
    let bare = tiles(&painted(None), cell);
    let label = |key| Some((Some(key), key));
    let left = painted(label("a"));
    let right = painted(label("d"));
    let y = left.dots[1].at.1 as usize + 1;
    let range = |scene: &Scene, i: usize| {
        let x = scene.dots[i].at.0 as usize;
        let cells = x - 1..=x + 1;
        assert!(cells.clone().all(|x| scene.text_cells.contains(&(x, y))));
        cells
    };
    let (a, d) = (range(&left, 1), range(&right, 4));
    let (left, right) = (tiles(&left, cell), tiles(&right, cell));
    let near = |r: Rect, x: &std::ops::RangeInclusive<usize>| {
        r.y.abs_diff(y) <= 1 && r.x <= x.end() + 1 && r.x + r.cells >= *x.start()
    };
    let changed = |from: &[(Rect, u64)], to: &[(Rect, u64)]| {
        from.iter()
            .filter(|t| !to.contains(t))
            .chain(to.iter().filter(|t| !from.contains(t)))
            .map(|t| t.0)
            .collect::<Vec<_>>()
    };
    let added = changed(&bare, &left);
    assert!(
        !added.is_empty() && added.iter().all(|&r| near(r, &a)),
        "{added:?}"
    );
    let moved = changed(&left, &right);
    assert!(
        moved.iter().any(|&r| near(r, &a)) && moved.iter().any(|&r| near(r, &d)),
        "{moved:?}"
    );
    assert!(
        moved.iter().all(|&r| near(r, &a) || near(r, &d)),
        "{moved:?}"
    );
}

#[test]
fn arrows_step_to_the_nearest_dot_that_way_ties_go_left_and_a_click_finds_a_dot_or_its_brackets() {
    use Toward::{Down, Left, Right, Up};
    let nodes = scene(SIX);
    let from = |picked, toward| {
        let topology = Topology {
            picked,
            ..view(&nodes, NOBODY, 120, GlyphSet::Unicode)
        };
        step(&topology, toward)
    };
    let (lead, coder) = (Some("agent-viz"), Some("agent-viz/coder"));
    assert_eq!(from(None, Down), Some(lead));
    assert_eq!(from(None, Up), None);
    assert_eq!(from(None, Left), Some(Some("agent-viz/helper")));
    assert_eq!(from(None, Right), Some(Some("agent-viz/plan-review-b")));
    assert_eq!(from(lead, Up), Some(None));
    assert_eq!(from(lead, Down), Some(Some("agent-viz/plan-review-a")));
    assert_eq!(from(lead, Left), Some(Some("agent-viz/helper")));
    assert_eq!(from(coder, Up), Some(lead));
    assert_eq!(from(coder, Right), Some(Some("agent-viz/helper")));
    assert_eq!(from(coder, Left), None);
    assert_eq!(from(Some("agent-viz/planner"), Down), None);
    let pair = scene("a=w a/b=d a/c=d");
    let topology = Topology {
        picked: Some("a"),
        ..view(&pair, NOBODY, 40, GlyphSet::Unicode)
    };
    assert_eq!(step(&topology, Down), Some(Some("a/b")), "the left one");
    let rows = paged(SIX, "agent-viz/coder", (120, 40), GlyphSet::Unicode);
    let y = rows.iter().position(|r| r.contains('[')).unwrap();
    let x = rows[y].chars().position(|c| c == '[').unwrap();
    let topology = view(&nodes, "agent-viz/coder", 120, GlyphSet::Unicode);
    let at = |place| key_at(&topology, 40 - CHROME_ROWS, place);
    for x in x..x + 3 {
        assert_eq!(at((x, y)), Some(coder), "{x}");
    }
    assert_eq!(at((x + 4, y)), None);
    let main = rows.iter().position(|r| r.contains('⠶')).unwrap();
    assert_eq!(at((60, main)), Some(None), "main");
}

#[test]
fn the_page_s_picture_is_cut_like_its_lines_with_dots_sized_from_their_gap() {
    let pictured = |spec: &str, picked, (width, height)| {
        let nodes = scene(spec);
        let topology = Topology {
            pictures: true,
            ..view(&nodes, picked, width, GlyphSet::Unicode)
        };
        let Drawing { lines, scenes, .. } = page(&topology, height - CHROME_ROWS);
        let [canvas, legend] = <[Scene; 2]>::try_from(scenes).unwrap();
        assert_eq!((0, legend.rows.end), (canvas.rows.start, lines.len()));
        assert_eq!(canvas.rows.end, legend.rows.start);
        assert!(legend.lines.is_empty(), "no line below the canvas");
        let tiles = tiles(&canvas, (9, 19));
        assert!(tiles.iter().all(|(rect, _)| rect.y < canvas.rows.end));
        let all = Scene {
            dots: [canvas.dots, legend.dots].concat(),
            ..canvas
        };
        (text(&lines), all)
    };
    let (_, tall) = pictured(TALL, "agent-viz", (80, 24));
    let bottom = tall.rows.end as f32;
    assert!(
        tall.lines.iter().flatten().any(|&(_, y)| y > bottom),
        "the tall tree runs past the canvas"
    );
    let (rows, six) = pictured(SIX, "agent-viz/coder", (120, 40));
    assert!(
        !rows.iter().any(|r| r.contains(['[', ']', '●', '⡇'])),
        "{rows:#?}"
    );
    let picked: Vec<_> = six.dots.iter().filter(|d| d.picked).collect();
    assert_eq!(picked.len(), 1);
    let braille = paged(SIX, "agent-viz/coder", (120, 40), GlyphSet::Unicode);
    let y = braille.iter().position(|r| r.contains('[')).unwrap();
    let x = braille[y].chars().position(|c| c == '[').unwrap() + 1;
    assert_eq!(picked[0].at, centre((x, y)));
    assert_eq!(picked[0].radius, PAGE_DOT, "room to spare");
    let main = six.dots.iter().find(|d| d.mark == Mark::Root);
    assert_eq!(main.map(|d| d.radius), Some(PAGE_DOT + MAIN_MORE));
    let legend = six.dots.iter().filter(|d| d.radius == LEGEND_DOT);
    assert!(legend.clone().all(|d| d.at.1 as usize == rows.len() - 1));
    assert_eq!(legend.count(), 6);
    let (_, twenty) = pictured(TWENTY, "resume-list-speed/coder", (40, 30));
    let agent = twenty.dots.iter().find(|d| d.halo && d.mark != Mark::Root);
    let two_cells = 1.0 - GAP_LESS;
    assert_eq!(
        agent.map(|d| d.radius),
        Some(two_cells),
        "5 px at a 2-cell gap"
    );
    let (rows, crowd) = pictured(CROWD, "sse-reconnect/coder", (40, 30));
    let nodes = scene(CROWD).len() + 1 + 6;
    assert!(crowd.dots.len() < nodes, "{} of {nodes}", crowd.dots.len());
    assert!(crowd.dots.iter().all(|d| d.at.0 > 0.0 && d.at.0 < 40.0));
    let y = rows.iter().position(|r| r.contains('▸')).unwrap();
    assert!(
        crowd.text_cells.contains(&(39, y)),
        "the scroll mark stays text"
    );
}

fn flow(from: Option<&str>, to: Option<&str>, at_ms: u64) -> SlotFlow {
    SlotFlow {
        from: from.map(str::to_owned),
        to: to.map(str::to_owned),
        at_ms,
    }
}

fn lit(spec: &str, flows: &[SlotFlow], now_ms: u64, policy: MotionPolicy) -> Scene {
    let nodes = scene(spec);
    let view = Topology {
        flows,
        now_ms,
        pictures: true,
        cx: MotionCtx::new(policy, GlyphSet::Unicode, Depth::TrueColor),
        ..view(&nodes, NOBODY, 35, GlyphSet::Unicode)
    };
    panel(&view).scenes.remove(0)
}

#[test]
fn a_light_eases_along_the_lines_from_sender_to_receiver_and_rings_there_for_280_ms() {
    let family = "a=w a/b=w a/c=d";
    let siblings = [flow(Some("a/b"), Some("a/c"), 1000)];
    let at = |ms| lit(family, &siblings, ms, MotionPolicy::Full);
    let start = at(1000);
    let dot = |key: usize| start.dots[key].at;
    let [light] = &start.lights[..] else {
        panic!("{:?}", start.lights);
    };
    assert_eq!(light.path.len(), 4, "over the crossbar, not up to a");
    assert_eq!((light.path[0], light.path[3]), (dot(2), dot(3)));
    assert_eq!((light.path[1].0, light.path[2].0), (dot(2).0, dot(3).0));
    assert_eq!(light.along, 0.0);
    assert_eq!(
        at(1150).lights[0].along,
        smooth(0.25),
        "600 ms for a short line"
    );
    assert_eq!(at(1300).lights[0].along, 0.5);
    let landed = at(1600);
    assert!(landed.lights.is_empty());
    assert_eq!(
        landed.rings,
        [Ring {
            on: start.dots[3],
            open: 0.0
        }]
    );
    assert_eq!(at(1740).rings[0].open, 0.5);
    assert!(!at(1880).moving(), "gone after REVEAL");
    let report = [flow(Some("a/c"), None, 1000)];
    let up = &lit(family, &report, 1000, MotionPolicy::Full).lights[0];
    assert_eq!(up.path.first(), Some(&dot(3)));
    assert_eq!(up.path.last(), Some(&dot(0)), "to main");
    let calm = lit(family, &siblings, 1600, MotionPolicy::Calm);
    assert_eq!(calm.lights[0].along, 0.5, "Calm doubles the flight");
    let still = lit(family, &siblings, 1300, MotionPolicy::Reduced);
    assert!(!still.moving(), "Reduced draws no light and no ring");

    let nodes = scene(family);
    let paged = |now_ms| {
        let topology = Topology {
            flows: &siblings,
            now_ms,
            pictures: true,
            cx: MotionCtx::new(MotionPolicy::Full, GlyphSet::Unicode, Depth::TrueColor),
            ..view(&nodes, "a/b", 80, GlyphSet::Unicode)
        };
        page(&topology, 20)
            .scenes
            .iter()
            .map(Scene::moving)
            .collect::<Vec<_>>()
    };
    assert_eq!(paged(1300), [true, false], "the legend never moves");
    assert_eq!(paged(2500), [false, false]);
}

#[test]
fn folded_ends_land_on_their_fold_and_past_three_in_flight_a_flow_only_rings() {
    let spec = f1("");
    let folded = lit(&spec, &[], 0, MotionPolicy::Full);
    let fold = folded.dots.iter().find(|d| d.radius == DOT - FOLDED_LESS);
    let b = fold.unwrap().at;
    let from_fold = [flow(Some("task-b/c05"), Some("helper"), 1000)];
    let light = &lit(&spec, &from_fold, 1000, MotionPolicy::Full).lights[0];
    assert_eq!(light.path[0], b, "from task-b's folded dot");
    let inside = [flow(Some("task-b/c01"), Some("task-b/c02"), 1000)];
    let one_dot = lit(&spec, &inside, 1100, MotionPolicy::Full);
    assert!(one_dot.lights.is_empty());
    assert_eq!(one_dot.rings.len(), 1, "a one-dot flow rings at once");
    assert_eq!(one_dot.rings[0].on.at, b);

    let ends = ["helper", "merge", "task-a", "task-c"];
    let crowd: Vec<SlotFlow> = (ends.iter())
        .map(|&to| flow(Some("task-a/coder/helper"), Some(to), 1000))
        .collect();
    let busy = lit(&spec, &crowd, 1100, MotionPolicy::Full);
    assert_eq!(busy.lights.len(), 3);
    assert_eq!(busy.rings.len(), 1, "the fourth rings its receiver");
    let ringing = busy.rings[0].on.at;
    assert!(busy.lights.iter().all(|l| l.path.last() != Some(&ringing)));
    assert!(busy.rings[0].open > 0.0, "since its at-ms");
    let later = flow(Some("task-a/coder/helper"), Some("merge"), 5000);
    let after = lit(
        &spec,
        &[crowd, vec![later]].concat(),
        5100,
        MotionPolicy::Full,
    );
    assert_eq!(after.lights.len(), 1, "landed lights free their places");
}

const BEND: &str = r#"
        ●          ●          ●          ●          ●          ●          ●          ●          ●          ●
       ⠒⡗⠒⡖⠒⡆     ⠒⡗⠒⡖⠒⡆     ⠒⡗⠒⡖⠒⡆     ⠒⡗⠒⡖⠒⡆     ⠒⡗⠒⡖⠒⡆     ⠒⡗⠒⡖⠒⡆     ⠒⡗⠒⡖⠒⣆     ⠒⡗⠒⡖⠒⣦     ⠒⡗⠒⡖⠒⡷     ⠒⡗⠒⡖⠗⡆
        ⡇ ⡇ ⣇      ⡇ ⡇ ⣇      ⡇ ⡇ ⣇      ⡇ ⡇ ⣧      ⡇ ⡇ ⡷      ⡇ ⡇ ⡟      ⡇ ⡇ ⡏      ⡇ ⡇ ⡇      ⡇ ⡇ ⡇      ⡇ ⡇ ⡇
        ✓ ● ✓      ✓ ● ✓      ✓ ● ✓      ✓ ● ✓      ✓ ● ✓      ✓ ● ✓      ✓ ● ✓      ✓ ● ✓      ✓ ● ✓      ✓ ● ✓

        ●          ●          ●          ●          ●          ●          ●          ●          ●⠐         ●⠐
       ⠒⡗⠒⡾⠒⡆     ⠒⡗⠒⡗⠒⡆     ⠒⡗⠺⡖⠒⡆     ⠒⡗⠗⡖⠒⡆     ⠒⡿⠒⡖⠒⡆     ⠒⡗⠒⡖⠒⡆     ⠒⡟⠒⡖⠒⡆     ⠒⡟⠒⡖⠒⡆     ⠒⡗⠒⡖⠒⡆     ⠒⡗⠒⡖⠒⡆
        ⡇ ⡇ ⡇      ⡇ ⡇ ⡇      ⡇ ⡇ ⡇      ⡇ ⡇ ⡇      ⡇ ⡇ ⡇      ⡇ ⡇ ⡇      ⡇ ⡇ ⡇      ⡇ ⡇ ⡇      ⡇ ⡇ ⡇      ⡇ ⡇ ⡇
        ✓ ● ✓      ✓ ● ✓      ✓ ● ✓      ✓ ● ✓      ✓ ● ✓      ✓ ● ✓      ✓ ● ✓      ✓ ● ✓      ✓ ● ✓      ✓ ● ✓

        ●⠰         ●⡰        ⢀●⡰        ⢄●⡰        ⢆●⡰        ⢆●⡰        ⢎●⡰         ●
       ⠒⡗⠒⡖⠒⡆     ⠒⡗⠒⡖⠒⡆     ⠒⡗⠒⡖⠒⡆     ⠒⡗⠒⡖⠒⡆     ⠒⡗⠒⡖⠒⡆     ⠒⡗⠒⡖⠒⡆     ⠒⡗⠒⡖⠒⡆     ⠒⡗⠒⡖⠒⡆
        ⡇ ⡇ ⡇      ⡇ ⡇ ⡇      ⡇ ⡇ ⡇      ⡇ ⡇ ⡇      ⡇ ⡇ ⡇      ⡇ ⡇ ⡇      ⡇ ⡇ ⡇      ⡇ ⡇ ⡇
        ✓ ● ✓      ✓ ● ✓      ✓ ● ✓      ✓ ● ✓      ✓ ● ✓      ✓ ● ✓      ✓ ● ✓      ✓ ● ✓
"#;

const DROP: &str = r#"
        ●      ●      ●      ●      ●      ●      ●      ●      ●      ●      ●      ●      ●      ●
        ⡇      ⡇      ⡇      ⡇      ⡇      ⡇      ⡇      ⡇      ⡇      ⣇      ⣇      ⣧      ⣧      ⡷
        ⣇      ⣇      ⣇      ⣇      ⣧      ⣧      ⡷      ⡷      ⡟      ⡏      ⡏      ⡇      ⡇      ⡇
        ✓      ✓      ✓      ✓      ✓      ✓      ✓      ✓      ✓      ✓      ✓      ✓      ✓      ✓

        ●      ●      ●      ●      ●⠐     ●⠐     ●⠰     ●⡰    ⢀●⡰    ⢄●⡰    ⢆●⡰    ⢆●⡰    ⢎●⡰     ●
        ⡷      ⡟      ⡟      ⡟      ⡇      ⡇      ⡇      ⡇      ⡇      ⡇      ⡇      ⡇      ⡇      ⡇
        ⡇      ⡇      ⡇      ⡇      ⡇      ⡇      ⡇      ⡇      ⡇      ⡇      ⡇      ⡇      ⡇      ⡇
        ✓      ✓      ✓      ✓      ✓      ✓      ✓      ✓      ✓      ✓      ✓      ✓      ✓      ✓
"#;

fn film(spec: &str, sent: SlotFlow, (x, y, w): (usize, usize, usize), group: usize) -> Vec<String> {
    let nodes = scene(spec);
    let flows = [sent];
    let shot = |k: u64| {
        let topology = Topology {
            flows: &flows,
            now_ms: 1000 + (k * 100 + 1) / 3,
            cx: MotionCtx::new(MotionPolicy::Full, GlyphSet::Unicode, Depth::TrueColor),
            ..view(&nodes, NOBODY, 35, GlyphSet::Unicode)
        };
        let rows = text(&panel(&topology).lines);
        let cut = |row: &String| -> String {
            row.chars()
                .chain(std::iter::repeat(' '))
                .skip(x)
                .take(w)
                .collect()
        };
        rows[y..y + 4].iter().map(cut).collect::<Vec<String>>()
    };
    let shots: Vec<Vec<String>> = (0..28).map(shot).collect();
    let mut out = Vec::new();
    for frames in shots.chunks(group) {
        for row in 0..4 {
            let cells: String = frames.iter().map(|f| format!("{}   ", f[row])).collect();
            out.push(format!("{:7}{}", "", cells).trim_end().to_owned());
        }
        out.push(String::new());
    }
    out.pop();
    out
}

#[test]
fn a_bump_slides_a_dot_at_a_time_round_corners_and_a_ring_opens_on_landing() {
    let review = flow(Some("task-a/review"), Some("task-a"), 1000);
    let spec = f1("task-a/review=d task-a/coder/helper=d");
    let bend = film(&spec, review, (14, 3, 8), 10);
    assert_eq!(bend, want(BEND));
    assert!(bend.iter().all(|row| !row.contains('⠿')), "one column wide");
    let helper = flow(Some("task-a/coder/helper"), Some("task-a/coder"), 1000);
    assert_eq!(film(&spec, helper, (10, 6, 4), 14), want(DROP));
}

fn look_at(lines: &[StyledLine], (x, y): (usize, usize)) -> Option<Look> {
    let spans = lines.get(y)?.spans.iter();
    let mut cells = spans.flat_map(|span| span.text.chars().map(move |_| span.look));
    cells.nth(x)
}

fn lit_at(flows: &[SlotFlow], now_ms: u64, set: GlyphSet, depth: Depth) -> Topology<'_> {
    Topology {
        flows,
        now_ms,
        cx: MotionCtx::new(MotionPolicy::Full, set, depth),
        ..view(&[], NOBODY, 35, set)
    }
}

#[test]
fn on_the_page_a_ring_covers_the_picked_brackets_and_they_return() {
    let nodes = scene(SIX);
    let flows = [flow(Some("agent-viz"), Some("agent-viz/coder"), 1000)];
    let at = |ms: u64, set| {
        let topology = Topology {
            nodes: &nodes,
            picked: Some("agent-viz/coder"),
            layout: Layout::new(120, set),
            ..lit_at(&flows, 1000 + ms, set, Depth::TrueColor)
        };
        page(&topology, 40 - CHROME_ROWS)
    };
    let row = |ms, set, y: usize| -> String {
        let rows = text(&at(ms, set).lines);
        let row: String = rows[y].chars().skip(11).take(25).collect();
        row.trim_end().to_owned()
    };
    let head = |ms, set| text(&at(ms, set).lines)[14].chars().nth(60);
    assert_eq!(head(0, GlyphSet::Unicode), Some('⡏'));
    assert_eq!(head(0, GlyphSet::Ascii), Some('#'));
    assert_eq!(row(600, GlyphSet::Unicode, 16), " ⡖⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠺⠒");
    assert_eq!(row(600, GlyphSet::Ascii, 16), " .---------------------#-");
    assert_eq!(row(1440, GlyphSet::Unicode, 20), "⢎●⡰");
    assert_eq!(row(1440, GlyphSet::Ascii, 20), "(@)");
    assert_eq!(row(1500, GlyphSet::Unicode, 20), "[●]");
    let Drawing { lines, moving, .. } = at(1440, GlyphSet::Unicode);
    let band = look_at(&lines, (11, 20)).and_then(|look| look.bg);
    assert_eq!(band, Some(Ink::from(Token::AccentTint)), "the band stays");
    assert!(moving);
    assert!(
        !at(1500, GlyphSet::Unicode).moving,
        "no frames once the ring has gone"
    );
}

#[test]
fn rings_give_way_to_counts_and_a_shared_cell_shows_the_right_dot_s_ring() {
    let ringing = |spec: &str, flows: &[SlotFlow], ms, y: usize| -> String {
        let nodes = scene(spec);
        let topology = Topology {
            nodes: &nodes,
            ..lit_at(flows, ms, GlyphSet::Unicode, Depth::TrueColor)
        };
        text(&panel(&topology).lines)[y].clone()
    };
    let folds = [
        flow(None, Some("task-a/coder"), 1000),
        flow(None, Some("task-b/c05"), 1000),
    ];
    let busy = busy("task-c/r3=f");
    assert_eq!(
        ringing(&busy, &folds, 1840, 3),
        " ●  ○ ⢎●+7⢎✓+30          ○"
    );
    let siblings = [
        flow(Some("task-a"), Some("task-a/helper"), 1000),
        flow(Some("task-a"), Some("task-a/planner"), 1000),
    ];
    let family = ringing(&f1(""), &siblings, 1840, 6);
    assert_eq!(family, "           ●  ⢎✓⢎●⡰○          ○  ✓");
    let ends = ["helper", "merge", "task-a", "task-c"];
    let crowd: Vec<SlotFlow> = (ends.iter())
        .map(|&to| flow(Some("task-a/coder/helper"), Some(to), 1000))
        .collect();
    let overflow = ringing(&f1(""), &crowd, 1100, 3);
    assert_eq!(overflow.chars().nth(32), Some('⡰'), "{overflow}");
}

#[test]
fn each_colour_tier_draws_the_bump_and_only_full_colour_fades_its_trail() {
    let nodes = scene(&f1(""));
    let review = [flow(Some("task-a/review"), Some("task-a"), 1000)];
    let trail = [(17, 4), (18, 4), (19, 4)];
    for depth in [Depth::TrueColor, Depth::Ansi256, Depth::Ansi16, Depth::None] {
        let theme = Theme::new(Mode::Dark, depth);
        let topology = Topology {
            nodes: &nodes,
            ..lit_at(&review, 1433, GlyphSet::Unicode, depth)
        };
        let lines = panel(&topology).lines;
        assert_eq!(text(&lines)[4].chars().nth(16), Some('⠗'), "{depth:?}");
        let style = |at| theme.style(&look_at(&lines, at).unwrap());
        let line = theme.style(&wire());
        let bump = style((16, 4));
        let fades: Vec<_> = trail.into_iter().map(style).collect();
        match depth {
            Depth::TrueColor | Depth::Ansi256 => {
                assert_ne!(bump, line, "{depth:?}");
                assert_ne!(fades[0], line, "{depth:?}");
                assert_ne!(fades[0], bump, "{depth:?}");
                let inks = trail.map(|at| look_at(&lines, at).and_then(|look| look.fg));
                let toward = |u| Some(Ink::mix(Token::Gleam, Token::Line, u, Token::Line));
                assert_eq!(
                    inks,
                    [toward(0.75), toward(0.5), toward(0.25)],
                    "back to grey"
                );
            }
            Depth::Ansi16 => {
                assert_ne!(bump, line, "the bead");
                assert!(fades.iter().all(|fade| *fade == line), "no trail");
            }
            Depth::None => assert!(
                [bump, line]
                    .into_iter()
                    .chain(fades)
                    .all(|s| s.fg.is_none()),
                "shapes only"
            ),
        }
    }
    let ascii = Topology {
        nodes: &nodes,
        layout: Layout::new(35, GlyphSet::Ascii),
        ..lit_at(&review, 1433, GlyphSet::Ascii, Depth::TrueColor)
    };
    assert_eq!(text(&panel(&ascii).lines)[4].chars().nth(16), Some('#'));
    let landed = Topology {
        now_ms: 1840,
        ..ascii
    };
    assert_eq!(&text(&panel(&landed).lines)[3][14..17], "(@)");
    let ringing = Topology {
        nodes: &nodes,
        ..lit_at(&review, 1840, GlyphSet::Unicode, Depth::TrueColor)
    };
    let half = look_at(&panel(&ringing).lines, (16, 3)).and_then(|look| look.fg);
    assert_eq!(half, Some(Ink::from(Token::Gleam)), "the light's ink");
}

#[test]
fn working_dots_glow_on_the_page_after_their_light_lands_and_only_from_256_colours() {
    let nodes = scene(SIX);
    let flows = [flow(Some("agent-viz/coder"), Some("agent-viz"), 1000)];
    let tint = |u| Some(Ink::mix(Token::HueCool, Token::Bg, u, Token::Bg));
    let glows = |ms: u64, depth| {
        let topology = Topology {
            nodes: &nodes,
            picked: Some("agent-viz/coder"),
            layout: Layout::new(120, GlyphSet::Unicode),
            ..lit_at(&flows, 1000 + ms, GlyphSet::Unicode, depth)
        };
        let lines = page(&topology, 40 - CHROME_ROWS).lines;
        (58..=62)
            .map(|x| look_at(&lines, (x, 13)).and_then(|l| l.bg))
            .collect::<Vec<_>>()
    };
    let lit = vec![None, tint(0.125), tint(0.25), tint(0.125), None];
    assert_eq!(glows(5000, Depth::TrueColor), lit);
    assert_eq!(glows(5000, Depth::Ansi256), lit);
    assert_eq!(glows(5000, Depth::Ansi16), [None; 5]);
    assert_eq!(glows(500, Depth::TrueColor), [None; 5], "the light first");
    let panel_bg = |depth| {
        let topology = Topology {
            nodes: &nodes,
            ..lit_at(&[], 0, GlyphSet::Unicode, depth)
        };
        (panel(&topology).lines.iter()).any(|l| l.spans.iter().any(|s| s.look.bg.is_some()))
    };
    assert!(!panel_bg(Depth::TrueColor), "never in the box");
    let cx = |policy| MotionCtx::new(policy, GlyphSet::Unicode, Depth::TrueColor);
    let second = Duration::from_secs(1);
    assert_eq!(tree_glow(second, cx(MotionPolicy::Full)), Some(0.15));
    assert_eq!(tree_glow(second * 2, cx(MotionPolicy::Calm)), Some(0.2));
    assert_eq!(tree_glow(second, cx(MotionPolicy::Reduced)), Some(0.25));
    let dim = MotionCtx::new(MotionPolicy::Full, GlyphSet::Unicode, Depth::Ansi16);
    assert_eq!(tree_glow(second, dim), None);
}
