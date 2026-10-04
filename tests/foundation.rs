use solapp::{SAApplication, SAContext, SAError, SAHost, SAHostConfig, SAStopProgress};
use std::time::Duration;

struct NeverStarted;
impl SAApplication for NeverStarted {
    type Message = String;
    type LocalEvent = u8;
    fn started(&mut self, _: &mut SAContext<'_, Self>) -> Result<(), SAError> {
        panic!("invalid config must not acquire a loop")
    }
    fn stopping(&mut self, _: &mut SAContext<'_, Self>) -> SAStopProgress {
        unreachable!()
    }
}

#[test]
fn rejected_host_configuration_preserves_owned_input_before_native_loop_acquisition() {
    for interval in [Duration::ZERO, Duration::from_secs(2)] {
        let config = SAHostConfig {
            stop_poll_interval: interval,
            ..SAHostConfig::default()
        };
        let rejection = match SAHost::<NeverStarted>::new(config.clone()) {
            Ok(_) => panic!("invalid configuration accepted"),
            Err(rejection) => rejection,
        };
        let (returned, reason) = rejection.into_parts();
        assert_eq!(returned, config);
        assert!(matches!(reason, SAError::InvalidInput(_)));
    }
}
