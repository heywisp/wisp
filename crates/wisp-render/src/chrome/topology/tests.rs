use std::time::Duration;

use wisp_protocol::agents::ROOT_NAME;
use wisp_protocol::slot::{NodeState, SlotTopologyNode};

use super::*;
use crate::canvas::Mark as Cell;
use crate::motion::MotionPolicy;
use crate::picture::Mark;
use crate::picture::{Rect, tiles};
use crate::testing::{crowded_agents, trimmed};
use crate::theme::glyphs::GlyphSet;
use crate::theme::{Depth, Mode, Theme};

const CHROME_ROWS: usize = 11;
const STILL: MotionCtx = MotionCtx {
    policy: MotionPolicy::Reduced,
    ascii: false,
    blend: false,
};
const NOBODY: &str = "nobody";

const TWELVE: &str = "helper=d merge=w task-a=w task-a/w1=w task-a/w2=o task-a/w3=d task-a/w4=w task-a/w5=d task-a/w6=w task-a/w7=o task-a/w8=w task-a/w9=d";

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
   ⡖⠒⠒⠒⠒⠒⡖⠒⠒⠒⠒⠒⠒⠒⠓⠒⠒⠒⠒⠒⡆
   ⡇     ⡇             ⡇
   ✓     ●             ●
             ⡖⠒⠒⡖⠒⡖⠒⠒⡖⠒⡗⠒⠒⡖⠒⡖⠒⠒⡖⠒⡆
             ⡇  ⡇ ⡇  ⡇ ⡇  ⡇ ⡇  ⡇ ⡇
             ●  ○ ✓  ● ✓  ● ○  ● ✓

⠶ main  ● working  ○ waiting
✓ done  ■ stopped  ⨯ failed
"#;

const BOX_ASCII: &str = r#"
                 *
   .-----.-------'-----.
   |     |             |
   +     @             @
             .--.-.--.-|--.-.--.-.
             |  | |  | |  | |  | |
             @  o +  @ +  @ o  @ +

* main  @ working  o waiting
+ done  = stopped  x failed
"#;

const BOX_40: &str = r#"
                  ⠶
   ⡖⡖⠒⠒⠒⠒⡖⠒⠒⠒⠒⠒⠒⡖⠒⠓⠒⠒⠒⠒⡖⠒⠒⠒⠒⠒⠒⡆
   ✓●    ●      ○      ✓      ●
      ⡖⡖⡖⡗⡖⡆ ⡖⡖⡖⡗⡖⡆ ⡖⡖⡖⡗⡖⡆ ⡖⡖⡖⡗⡖⡆
      ●○✓●■● ⨯✓●○✓● ■●⨯✓●○ ✓●■●⨯✓

⠶ main  ● working  ○ waiting
✓ done  ■ stopped  ⨯ failed
"#;

const BOX_CLOSE: &str = r#"
                 ⠶
  ⡖⡖⠒⠒⠒⠒⡖⠒⠒⠒⠒⠒⠒⡖⠒⠓⠒⠒⠒⠒⡖⠒⠒⠒⠒⠒⠒⡆
  ✓●    ●      ○      ✓      ●
     ⡖⡖⡖⡗⡖⡆ ⡖⡖⡖⡗⡖⡆ ⡖⡖⡖⡗⡖⡆ ⡖⡖⡖⡗⡖⡆
     ●○✓●■● ⨯✓●○✓● ■●⨯✓●○ ✓●■●⨯✓

⠶ main  ● working  ○ waiting
✓ done  ■ stopped  ⨯ failed
"#;

const BOX_CUT: &str = r#"
                             ⠶
⡖⡖⠒⠒⠒⠒⡖⠒⠒⠒⠒⠒⠒⡖⠒⠒⠒⠒⠒⠒⡖⠒⠒⠒⠒⠒⠒⡖⠒⠓⠒⠒⠒⠒
✓●    ●      ○      ✓      ●      ▸
   ⡖⡖⡖⡗⡖⡆ ⡖⡖⡖⡗⡖⡆ ⡖⡖⡖⡗⡖⡆ ⡖⡖⡖⡗⡖⡆ ⡖⡖⡖
   ●○✓●■● ⨯✓●○✓● ■●⨯✓●○ ✓●■●⨯✓ ●○✓

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
        pictures: None,
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

fn boxed(nodes: &[SlotTopologyNode], width: usize, set: GlyphSet) -> Vec<String> {
    text(&panel(&view(nodes, NOBODY, width, set)).lines)
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
    format!(
        "helper=w merge=o task-a=w task-a/coder=w task-a/coder/helper=w task-a/coder/helper/a=w task-a/coder/helper/b=d task-a/helper=d task-a/planner=w task-a/review=o task-b=d task-b/c01=d task-b/c02=d task-c=o task-c/coder=o task-c/review=d {changes}"
    )
}

#[test]
fn the_box_puts_main_on_the_first_row_and_each_generation_three_rows_down() {
    let tall = scene(TALL);
    for (width, rows) in [(35, [0, 3, 6, 9, 12, 15]), (6, [0, 2, 4, 6, 8, 10])] {
        let box_rows = boxed(&tall, width, GlyphSet::Unicode);
        assert!(box_rows[0].contains('⠶'));
        for row in &rows[1..] {
            assert!(box_rows[*row].contains('●'), "{box_rows:?}");
        }
    }
    for width in [20, 35, 120] {
        let rows = boxed(&scene(SIX), width, GlyphSet::Unicode);
        let legend = &rows[rows.len() - 2..];
        assert_eq!(legend[0].matches(['⠶', '●', '○']).count(), 3, "{rows:#?}");
        assert!(legend[1].starts_with("✓ done"), "{rows:#?}");
    }
}

#[test]
fn a_family_wider_than_the_box_packs_closer_and_past_that_is_cut_with_a_mark() {
    let twelve = scene(TWELVE);
    let thirty = crowded_agents(4);
    let fifty_eight = crowded_agents(8);
    for (nodes, width, set, frame) in [
        (&twelve, 35, GlyphSet::Unicode, BOX),
        (&twelve, 35, GlyphSet::Ascii, BOX_ASCII),
        (&thirty, 35, GlyphSet::Unicode, BOX_CLOSE),
        (&thirty, 36, GlyphSet::Unicode, BOX_40),
        (&fifty_eight, 35, GlyphSet::Unicode, BOX_CUT),
    ] {
        assert_eq!(boxed(nodes, width, set), want(frame));
    }
    let cut = panel(&view(&fifty_eight, NOBODY, 35, GlyphSet::Unicode));
    assert_eq!(
        look_at(&cut.lines, (34, 2)),
        Some(Look::fg(Token::TextFaint))
    );
    let family = scene("a=w a/0=d a/1=w a/2=d b=w b/0=w");
    let rows = boxed(&family, 7, GlyphSet::Unicode);
    let working = |y: usize| {
        rows[y]
            .match_indices('●')
            .map(|(i, _)| rows[y][..i].chars().count())
            .collect::<Vec<_>>()
    };
    assert_eq!(
        working(2),
        working(4),
        "odd and only children centre under their parent"
    );
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
        let placed = Placed::page(&tree, true, 40, 0, false, usize::MAX);
        let view = view(&nodes, NOBODY, 40, set);
        let mut drawn = draw(
            &placed,
            &view,
            &flights(&view, &tree, &placed),
            None,
            placed.width,
        );
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
fn a_sprite_step_changes_only_the_working_marks_own_tiles() {
    let nodes = scene("a=w b=d a/c=w a/d=o");
    let at = |ms| {
        let view = Topology {
            pictures: Some(9),
            at: Duration::from_millis(ms),
            cx: MotionCtx::new(MotionPolicy::Full, GlyphSet::Unicode, Depth::TrueColor),
            ..view(&nodes, NOBODY, 30, GlyphSet::Unicode)
        };
        panel(&view).scenes.remove(0)
    };
    let cell = (9, 19);
    let (before, after) = (at(199), at(200));
    assert_eq!(tiles(&at(0), cell), tiles(&before, cell), "a 200 ms grid");
    assert_eq!(
        tiles(&before, (4, 8)),
        tiles(&after, (4, 8)),
        "still pluses"
    );
    let working: Vec<(usize, usize)> = before
        .dots
        .iter()
        .filter(|d| d.radius != LEGEND_DOT && d.mark == Mark::State(NodeState::Working))
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
                pictures: Some(9),
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
    for (scene, i) in [(&left, 1), (&right, 4)] {
        let x = scene.dots[i].at.0 as usize;
        assert!((x - 1..=x + 1).all(|x| scene.text_cells.contains(&(x, y))));
        assert_eq!(tiles(scene, cell), bare, "the label is outside the drawing");
    }
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
            pictures: Some(9),
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
    let agent = twenty
        .dots
        .iter()
        .find(|d| d.radius != LEGEND_DOT && d.mark != Mark::Root);
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
    assert!(
        crowd
            .dots
            .iter()
            .any(|d| d.at.0 == 39.5 && d.mark == Mark::State(NodeState::Waiting))
    );
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
        pictures: Some(9),
        cx: MotionCtx::new(policy, GlyphSet::Unicode, Depth::TrueColor),
        ..view(&nodes, NOBODY, 35, GlyphSet::Unicode)
    };
    panel(&view).scenes.remove(0)
}

#[test]
fn a_light_steps_along_the_lines_from_sender_to_receiver_and_rings_there_for_280_ms() {
    let family = "a=w a/b=w a/c=d";
    let siblings = [flow(Some("a/b"), Some("a/c"), 1000)];
    let at = |ms| lit(family, &siblings, ms, MotionPolicy::Full);
    let start = at(1000);
    let dot = |key: usize| start.dots[key].at;
    let [light] = &start.lights[..] else {
        panic!("{:?}", start.lights);
    };
    assert_eq!(light.path.len(), 2, "the sender and the first line cell");
    let bar = start
        .lines
        .iter()
        .flatten()
        .find(|&&(x, y)| x == dot(2).0 && y < dot(2).1 && y > dot(1).1)
        .unwrap()
        .1;
    let line = [dot(2), (dot(2).0, bar), (dot(3).0, bar), dot(3)];
    let route = cells(&line.map(|(x, y)| (x as usize, y as usize)));
    assert_eq!(route.len(), 23, "over the crossbar, not up to a");
    for h in 0..21_u64 {
        let step = 1000 + h * 33;
        let head = h as usize + 1;
        let stretch: Vec<_> = route[head.saturating_sub(TRAIL)..=head]
            .iter()
            .copied()
            .map(centre)
            .collect();
        assert_eq!(at(step).lights[0].path, stretch, "at step {h}");
        assert_eq!(
            at(step + 32).lights[0].path,
            stretch,
            "before the next step"
        );
    }
    let landed = at(1693);
    assert!(landed.lights.is_empty());
    assert_eq!(
        landed.rings,
        [Ring {
            on: start.dots[3],
            step: 0
        }]
    );
    assert_eq!(at(1833).rings[0].step, 1);
    assert_eq!(at(1880).rings[0].step, 2);
    assert_eq!(
        tiles(&landed, (9, 19)),
        tiles(&at(1786), (9, 19)),
        "one ring step"
    );
    assert!(!at(1973).moving(), "gone after REVEAL");
    let report = [flow(Some("a/c"), None, 1000)];
    let up = |now| lit(family, &report, now, MotionPolicy::Full);
    assert_eq!(up(1000).lights[0].path.first(), Some(&dot(3)));
    let last = (1000..2200)
        .step_by(33)
        .map(up)
        .take_while(|s| !s.lights.is_empty())
        .last()
        .unwrap();
    assert_eq!(
        last.lights[0].path.last(),
        Some(&(dot(0).0, dot(0).1 + 1.0)),
        "to main"
    );
    let calm = |ms| lit(family, &siblings, ms, MotionPolicy::Calm);
    assert_eq!(
        calm(1065).lights[0].path,
        light.path,
        "Calm waits two frames"
    );
    assert_eq!(
        calm(1066).lights[0].path.last(),
        Some(&centre(route[2])),
        "the first step"
    );
    let still = lit(family, &siblings, 1300, MotionPolicy::Reduced);
    assert!(!still.moving(), "Reduced draws no light and no ring");
    let to_self = [flow(Some("a/b"), Some("a/b"), 1000)];
    let same = lit(family, &to_self, 1000, MotionPolicy::Full);
    assert!(same.lights.is_empty(), "no line cells to fly over");
    assert_eq!(
        same.rings,
        [Ring {
            on: start.dots[2],
            step: 0
        }]
    );

    let nodes = scene(family);
    let paged = |now_ms| {
        let topology = Topology {
            flows: &siblings,
            now_ms,
            pictures: Some(9),
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
fn past_three_in_flight_a_flow_only_rings() {
    let spec = f1("");
    let ends = ["helper", "merge", "task-a", "task-c"];
    let crowd: Vec<SlotFlow> = (ends.iter())
        .map(|&to| flow(Some("task-a/coder/helper"), Some(to), 1000))
        .collect();
    let busy = lit(&spec, &crowd, 1100, MotionPolicy::Full);
    assert_eq!(busy.lights.len(), 3);
    assert_eq!(busy.rings.len(), 1, "the fourth rings its receiver");
    let ringing = busy.rings[0].on.at;
    assert!(busy.lights.iter().all(|l| l.path.last() != Some(&ringing)));
    assert_eq!(busy.rings[0].step, 1, "since its at-ms");
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
       ⠒⡗⠒⡖⠒⡆     ⠒⡗⠒⡖⠒⡆     ⠒⡗⠒⡖⠒⡷     ⠒⡗⠒⡖⠒⡷     ⠒⡗⠒⡖⠗⡆     ⠒⡗⠒⡖⠗⡆     ⠒⡗⠒⡗⠒⡆     ⠒⡗⠒⡗⠒⡆     ⠒⡗⠗⡖⠒⡆     ⠒⡗⠗⡖⠒⡆
        ⡇ ⡇ ⡟      ⡇ ⡇ ⡟      ⡇ ⡇ ⡇      ⡇ ⡇ ⡇      ⡇ ⡇ ⡇      ⡇ ⡇ ⡇      ⡇ ⡇ ⡇      ⡇ ⡇ ⡇      ⡇ ⡇ ⡇      ⡇ ⡇ ⡇
        ✓ ● ✓      ✓ ● ✓      ✓ ● ✓      ✓ ● ✓      ✓ ● ✓      ✓ ● ✓      ✓ ● ✓      ✓ ● ✓      ✓ ● ✓      ✓ ● ✓

        ●          ●          ●⠐         ●⠐         ●⠰         ●⡰        ⢀●⡰        ⢄●⡰        ⢄●⡰        ⢆●⡰
       ⠒⡟⠒⡖⠒⡆     ⠒⡟⠒⡖⠒⡆     ⠒⡗⠒⡖⠒⡆     ⠒⡗⠒⡖⠒⡆     ⠒⡗⠒⡖⠒⡆     ⠒⡗⠒⡖⠒⡆     ⠒⡗⠒⡖⠒⡆     ⠒⡗⠒⡖⠒⡆     ⠒⡗⠒⡖⠒⡆     ⠒⡗⠒⡖⠒⡆
        ⡇ ⡇ ⡇      ⡇ ⡇ ⡇      ⡇ ⡇ ⡇      ⡇ ⡇ ⡇      ⡇ ⡇ ⡇      ⡇ ⡇ ⡇      ⡇ ⡇ ⡇      ⡇ ⡇ ⡇      ⡇ ⡇ ⡇      ⡇ ⡇ ⡇
        ✓ ● ✓      ✓ ● ✓      ✓ ● ✓      ✓ ● ✓      ✓ ● ✓      ✓ ● ✓      ✓ ● ✓      ✓ ● ✓      ✓ ● ✓      ✓ ● ✓

       ⢎●⡰         ●          ●          ●          ●          ●          ●          ●
       ⠒⡗⠒⡖⠒⡆     ⠒⡗⠒⡖⠒⡆     ⠒⡗⠒⡖⠒⡆     ⠒⡗⠒⡖⠒⡆     ⠒⡗⠒⡖⠒⡆     ⠒⡗⠒⡖⠒⡆     ⠒⡗⠒⡖⠒⡆     ⠒⡗⠒⡖⠒⡆
        ⡇ ⡇ ⡇      ⡇ ⡇ ⡇      ⡇ ⡇ ⡇      ⡇ ⡇ ⡇      ⡇ ⡇ ⡇      ⡇ ⡇ ⡇      ⡇ ⡇ ⡇      ⡇ ⡇ ⡇
        ✓ ● ✓      ✓ ● ✓      ✓ ● ✓      ✓ ● ✓      ✓ ● ✓      ✓ ● ✓      ✓ ● ✓      ✓ ● ✓
"#;

const DROP: &str = r#"
        ●      ●      ●      ●      ●      ●      ●      ●      ●      ●      ●      ●      ●⠐     ●⠐
        ⡇      ⡇      ⡇      ⡇      ⡇      ⡇      ⡟      ⡟      ⡟      ⡟      ⡟      ⡟      ⡇      ⡇
        ⡟      ⡟      ⡟      ⡟      ⡟      ⡟      ⡇      ⡇      ⡇      ⡇      ⡇      ⡇      ⡇      ⡇
        ✓      ✓      ✓      ✓      ✓      ✓      ✓      ✓      ✓      ✓      ✓      ✓      ✓      ✓

        ●⠰     ●⡰    ⢀●⡰    ⢄●⡰    ⢄●⡰    ⢆●⡰    ⢎●⡰     ●      ●      ●      ●      ●      ●      ●
        ⡇      ⡇      ⡇      ⡇      ⡇      ⡇      ⡇      ⡇      ⡇      ⡇      ⡇      ⡇      ⡇      ⡇
        ⡇      ⡇      ⡇      ⡇      ⡇      ⡇      ⡇      ⡇      ⡇      ⡇      ⡇      ⡇      ⡇      ⡇
        ✓      ✓      ✓      ✓      ✓      ✓      ✓      ✓      ✓      ✓      ✓      ✓      ✓      ✓
"#;

fn film(spec: &str, sent: SlotFlow, (x, y, w): (usize, usize, usize), group: usize) -> Vec<String> {
    let nodes = scene(spec);
    let flows = [sent];
    let shot = |k: u64| {
        let topology = Topology {
            flows: &flows,
            now_ms: 1000 + k * ms(FRAME_MIN),
            cx: MotionCtx::new(MotionPolicy::Full, GlyphSet::Unicode, Depth::TrueColor),
            ..view(&nodes, NOBODY, 34, GlyphSet::Unicode)
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
fn a_light_moves_a_whole_cell_a_step_round_corners_and_a_ring_opens_on_landing() {
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
fn a_long_route_moves_the_same_cells_every_frame_and_lands_within_1_2_s() {
    let nodes = scene(FLAT);
    let flows = [flow(
        Some(&nodes.first().unwrap().key),
        Some(&nodes.last().unwrap().key),
        1000,
    )];
    let (width, rows) = (200, 8);
    let set = GlyphSet::Unicode;
    let mut prev = None;
    for age in (0..=1200).step_by(33).chain([1200]) {
        let topology = Topology {
            nodes: &nodes,
            layout: Layout::new(width, set),
            ..lit_at(&flows, 1000 + age, set, Depth::TrueColor)
        };
        let lines = page(&topology, rows + 1).lines;
        let heads: Vec<_> = (1..rows - 1)
            .flat_map(|y| (0..width).map(move |x| (x, y)))
            .filter(|&at| look_at(&lines, at).and_then(|look| look.fg) == Some(LIGHT.into()))
            .collect();
        if age < 1056 {
            let &[(x, y)] = heads.as_slice() else {
                panic!("one bright cell at {age} ms, got {heads:?}");
            };
            if let Some((px, py)) = prev.replace((x, y)) {
                assert_eq!(x.abs_diff(px) + y.abs_diff(py), 6, "at {age} ms");
            }
        } else {
            assert!(heads.is_empty(), "no head after landing at {age} ms");
        }
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
    assert_eq!(head(0, GlyphSet::Unicode), Some('⣧'));
    assert_eq!(head(0, GlyphSet::Ascii), Some('#'));
    assert_eq!(row(600, GlyphSet::Unicode, 16), " ⡖⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠗⠒⠒⠒⠒⠒⠒⠒⠒⠒");
    assert_eq!(row(600, GlyphSet::Ascii, 16), " .-------------#---------");
    assert_eq!(row(1131, GlyphSet::Unicode, 20), "⢎●⡰");
    assert_eq!(row(1131, GlyphSet::Ascii, 20), "(@)");
    assert_eq!(row(1171, GlyphSet::Unicode, 20), "[●]");
    let Drawing { lines, moving, .. } = at(1131, GlyphSet::Unicode);
    let band = look_at(&lines, (11, 20)).and_then(|look| look.bg);
    assert_eq!(band, Some(Ink::from(Token::AccentTint)), "the band stays");
    assert!(moving);
    assert!(
        !at(1171, GlyphSet::Unicode).moving,
        "no frames once the ring has gone"
    );
}

#[test]
fn a_ring_never_covers_a_neighbour_s_mark_and_a_shared_cell_shows_the_right_dot_s_ring() {
    let nodes = scene("a=w a/0=d a/1=o b=w b/0=d b/1=d");
    let flows = [
        flow(Some("a/1"), Some("a/1"), 1000),
        flow(Some("b/0"), Some("b/0"), 1000),
    ];
    let drawing = panel(&Topology {
        nodes: &nodes,
        layout: Layout::new(7, GlyphSet::Unicode),
        ..lit_at(&flows, 1240, GlyphSet::Unicode, Depth::TrueColor)
    });
    assert_eq!(text(&drawing.lines)[4], " ✓○⢎✓✓");
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
            layout: Layout::new(34, GlyphSet::Unicode),
            ..lit_at(&review, 1264, GlyphSet::Unicode, depth)
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
        layout: Layout::new(34, GlyphSet::Ascii),
        ..lit_at(&review, 1264, GlyphSet::Ascii, Depth::TrueColor)
    };
    assert_eq!(text(&panel(&ascii).lines)[4].chars().nth(16), Some('#'));
    let landed = Topology {
        now_ms: 1636,
        ..ascii
    };
    assert_eq!(&text(&panel(&landed).lines)[3][14..17], "(@)");
    let ringing = Topology {
        nodes: &nodes,
        layout: Layout::new(34, GlyphSet::Unicode),
        ..lit_at(&review, 1636, GlyphSet::Unicode, Depth::TrueColor)
    };
    let half = look_at(&panel(&ringing).lines, (16, 3)).and_then(|look| look.fg);
    assert_eq!(half, Some(Ink::from(Token::Gleam)), "the light's ink");
}
