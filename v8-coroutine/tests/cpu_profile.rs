//! The CPU profiler samples every coroutine, not only the V8 thread identity that started it.
//!
//! Each coroutine has its own V8 thread identity, and V8's sampler used to keep the identity it
//! was created on: a profile started outside a coroutine then dropped every sample taken inside
//! one as "isolate not locked".

mod common;

use std::cell::RefCell;
use std::rc::Rc;

use common::{Js, finish, run, start};
use v8::inspector::{
    Channel, ChannelImpl, StringBuffer, StringView, V8Inspector, V8InspectorClient, V8InspectorClientImpl,
    V8InspectorClientTrustLevel, V8InspectorSession,
};

struct NoClient;

impl V8InspectorClientImpl for NoClient {}

struct Sink(Rc<RefCell<Vec<String>>>);

impl ChannelImpl for Sink {
    fn send_response(&self, _: i32, message: v8::UniquePtr<StringBuffer>) {
        if let Some(message) = message.as_ref() {
            let view = message.string();
            let text = match (view.characters8(), view.characters16()) {
                (Some(bytes), _) => bytes.iter().map(|&b| b as char).collect(),
                (_, Some(units)) => String::from_utf16_lossy(units),
                _ => String::new(),
            };
            self.0.borrow_mut().push(text);
        }
    }

    fn send_notification(&self, _: v8::UniquePtr<StringBuffer>) {}

    fn flush_protocol_notifications(&self) {}
}

fn post(session: &V8InspectorSession, message: &str) {
    let units: Vec<u16> = message.encode_utf16().collect();
    session.dispatch_protocol_message(StringView::from(&units[..]));
}

/// Hits recorded in `profile` for functions named `name`.
fn hits(profile: &str, name: &str) -> u64 {
    let needle = format!("\"functionName\":\"{name}\"");
    profile
        .split("{\"id\":")
        .filter(|node| node.contains(&needle))
        .filter_map(|node| node.split("\"hitCount\":").nth(1))
        .filter_map(|rest| rest.split(|c: char| !c.is_ascii_digit()).next()?.parse::<u64>().ok())
        .sum()
}

#[test]
fn samples_a_coroutine_other_than_the_one_that_started_the_profile() {
    common::init("");
    let js = Js::new();
    let responses = Rc::new(RefCell::new(Vec::new()));
    let (inspector, session) = {
        let mut locker = js.isolate.lock();
        v8::scope!(let scope, &mut *locker);
        let context = v8::Local::new(scope, &*js.context);
        let scope = &mut v8::ContextScope::new(scope, context);
        let inspector = V8Inspector::create(scope, V8InspectorClient::new(Box::new(NoClient)));
        inspector.context_created(context, 1, StringView::from(&b"test"[..]), StringView::empty());
        let channel = Channel::new(Box::new(Sink(responses.clone())));
        let session = inspector.connect(1, channel, StringView::empty(), V8InspectorClientTrustLevel::FullyTrusted);
        post(&session, r#"{"id":1,"method":"Profiler.enable"}"#);
        post(&session, r#"{"id":2,"method":"Profiler.setSamplingInterval","params":{"interval":100}}"#);
        post(&session, r#"{"id":3,"method":"Profiler.start"}"#);
        (inspector, session)
    };

    // Busy for 300 ms on a coroutine, across one suspend so it also runs on a restored state.
    let busy = "function hot(ms) { const end = Date.now() + ms; let n = 0; \
                while (Date.now() < end) n++; return n; } \
                hot(150); suspend(); String(hot(150))";
    let mut co = js.coroutine(busy);
    start(&mut co);
    assert!(finish(&mut co, "").is_ok());
    drop(co.into_stack());

    let profile = {
        let mut locker = js.isolate.lock();
        v8::scope!(let scope, &mut *locker);
        let context = v8::Local::new(scope, &*js.context);
        let scope = &mut v8::ContextScope::new(scope, context);
        post(&session, r#"{"id":4,"method":"Profiler.stop"}"#);
        // Keep the context alive for the session's last response.
        run(scope, "0");
        responses.borrow().iter().find(|r| r.starts_with("{\"id\":4,")).cloned().expect("Profiler.stop response")
    };
    drop(session);
    drop(inspector);

    // About 3,000 samples land in `hot`; before the fix there were none.
    let hot = hits(&profile, "hot");
    assert!(hot > 300, "only {hot} samples in the coroutine's `hot`");
}
