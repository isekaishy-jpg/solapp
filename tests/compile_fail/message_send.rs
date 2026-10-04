use std::rc::Rc;
use solapp::{SAApplication, SAContext, SAError, SAStopProgress};
struct App;
impl SAApplication for App {
    type Message = Rc<()>;
    type LocalEvent = ();
    fn started(&mut self, _: &mut SAContext<'_, Self>) -> Result<(), SAError> { Ok(()) }
    fn stopping(&mut self, _: &mut SAContext<'_, Self>) -> SAStopProgress { SAStopProgress::Settled }
}
fn main() {}
