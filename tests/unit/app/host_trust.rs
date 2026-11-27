use super::*;

impl App {
    pub(in crate::app) fn approve_fixture_host(&mut self) -> bool {
        let Some(prompt) = self.host_trust.pending.pop_front() else {
            return false;
        };
        assert!(prompt.trusted.is_none());
        assert!(prompt.presented.fingerprint.starts_with("SHA256:"));
        self.finish_host_trust(prompt, true);
        true
    }
}

impl App {
    pub(in crate::app) fn deny_fixture_replacement(&mut self, expected: &Key) {
        let prompt = self.host_trust.pending.pop_front().unwrap();
        assert!(prompt.trusted.is_some());
        assert_eq!(&prompt.presented, expected);
        self.finish_host_trust(prompt, false);
    }
}
