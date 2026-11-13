use crate::backend::Backend;
use crate::config::Settings;
use crate::fake::FakeBackend;
use crate::real::RealBackend;

pub(crate) struct Context {
    pub(crate) settings: Settings,
    pub(crate) backend: Box<dyn Backend>,
}

impl Context {
    pub(crate) fn new(settings: Settings, backend: Box<dyn Backend>) -> Context {
        Context { settings, backend }
    }

    pub(crate) fn from_env() -> Context {
        let settings = Settings::from_env();
        let backend: Box<dyn Backend> = if settings.fake {
            Box::new(FakeBackend::from_env())
        } else {
            Box::new(RealBackend::system())
        };
        Context::new(settings, backend)
    }
}
