use crate::backend::Backend;
use crate::config::Settings;

pub(crate) struct Context {
    pub(crate) settings: Settings,
    pub(crate) backend: Box<dyn Backend>,
}

impl Context {
    pub(crate) fn new(settings: Settings, backend: Box<dyn Backend>) -> Context {
        Context { settings, backend }
    }
}
