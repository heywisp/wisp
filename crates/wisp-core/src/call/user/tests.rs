use super::*;

#[test]
fn every_reply_keeps_its_variant_and_its_answers_through_the_plugin_conversion() {
    let answers = vec![
        Answer {
            chosen: vec![0, 2],
            own: None,
        },
        Answer {
            chosen: Vec::new(),
            own: Some("own words".to_owned()),
        },
    ];
    let cases = [
        (
            AskReply::Answered { answers },
            api::AskReply::Answered(vec![
                api::Answer {
                    chosen: vec![0, 2],
                    own: None,
                },
                api::Answer {
                    chosen: Vec::new(),
                    own: Some("own words".to_owned()),
                },
            ]),
        ),
        (
            AskReply::Attached { typed: true },
            api::AskReply::Attached(true),
        ),
        (AskReply::Declined, api::AskReply::Declined),
        (
            AskReply::ProcessEnded { typed: false },
            api::AskReply::ProcessEnded(false),
        ),
        (AskReply::MessageInstead, api::AskReply::MessageInstead),
        (AskReply::Unavailable, api::AskReply::Unavailable),
    ];
    for (reply, expected) in cases {
        assert_eq!(api::AskReply::from(reply), expected);
    }
}
