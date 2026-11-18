use super::*;

#[test]
fn only_a_call_with_a_token_and_a_sidebar_render_see_the_agents() {
    let token = CancellationToken::new();
    assert!(Caller::call(token.clone(), None).sees_agents());
    assert!(
        Caller::call(token.clone(), None)
            .hook(1, At::default())
            .sees_agents()
    );
    assert!(Caller::rendering(Render::Slot).sees_agents());
    assert!(!Caller::default().sees_agents());
    assert!(!Caller::rendering(Render::Start).sees_agents());
    let start_with_token = Caller {
        cancel: Some(token),
        ..Caller::rendering(Render::Start)
    };
    assert!(!start_with_token.sees_agents());
}

#[test]
fn sync_scope_sets_the_caller_for_its_closure_only() {
    let token = CancellationToken::new();
    assert!(!Caller::current().sees_agents());
    let inside = Caller::call(token, None).sync_scope(|| Caller::current().sees_agents());
    assert!(inside);
    assert!(!Caller::current().sees_agents());
    let token = CancellationToken::new();
    let nested = Caller::call(token, None)
        .sync_scope(|| Caller::default().sync_scope(|| Caller::current().sees_agents()));
    assert!(!nested);
}
