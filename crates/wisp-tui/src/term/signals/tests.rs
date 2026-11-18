use signal_hook::consts::{SIGCONT, SIGHUP, SIGTERM, SIGTSTP};

use super::deaths_first;

#[test]
fn a_death_in_the_same_batch_runs_before_a_continue_or_a_stop() {
    assert_eq!(deaths_first([SIGCONT, SIGTERM]), [SIGTERM, SIGCONT]);
    assert_eq!(
        deaths_first([SIGTSTP, SIGCONT, SIGHUP]),
        [SIGHUP, SIGTSTP, SIGCONT]
    );
    assert_eq!(deaths_first([SIGCONT, SIGTSTP]), [SIGCONT, SIGTSTP]);
    assert_eq!(deaths_first([SIGHUP, SIGTERM]), [SIGHUP, SIGTERM]);
    assert!(deaths_first([]).is_empty());
}

#[test]
fn a_death_or_a_continue_that_arrives_before_the_stop_is_handled_instead() {
    use super::{Settled, Then, before_stopping};

    for then in [Then::Stop, Then::StopInBackground] {
        assert_eq!(
            before_stopping(then, [SIGCONT, SIGTERM], false),
            Settled {
                stop: false,
                next: vec![SIGTERM, SIGCONT]
            }
        );
        assert_eq!(
            before_stopping(then, [SIGHUP], false),
            Settled {
                stop: false,
                next: vec![SIGHUP]
            }
        );
        assert_eq!(
            before_stopping(then, [SIGCONT], false),
            Settled {
                stop: false,
                next: vec![SIGCONT]
            }
        );
        assert_eq!(
            before_stopping(then, [], false),
            Settled {
                stop: true,
                next: vec![]
            }
        );
        assert_eq!(
            before_stopping(then, [SIGTSTP], false),
            Settled {
                stop: true,
                next: vec![SIGTSTP]
            }
        );
    }
    assert_eq!(
        before_stopping(Then::StopInBackground, [], true),
        Settled {
            stop: false,
            next: vec![SIGCONT]
        }
    );
    assert_eq!(
        before_stopping(Then::Stop, [], true),
        Settled {
            stop: true,
            next: vec![]
        }
    );
    assert_eq!(
        before_stopping(Then::Run, [SIGTSTP], false),
        Settled {
            stop: false,
            next: vec![SIGTSTP]
        }
    );
}
