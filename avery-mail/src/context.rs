use crate::backend::Backend;
use crate::calendar::{CalendarBackend, FakeCalendar, RealCalendar};
use crate::config::Settings;
use crate::fake::FakeBackend;
#[cfg(test)]
use crate::fake::FakeOptions;
use crate::real::RealBackend;

pub(crate) struct Context {
    pub(crate) settings: Settings,
    pub(crate) backend: Box<dyn Backend>,
    pub(crate) calendar: Box<dyn CalendarBackend>,
}

impl Context {
    pub(crate) fn new(
        settings: Settings,
        backend: Box<dyn Backend>,
        calendar: Box<dyn CalendarBackend>,
    ) -> Context {
        Context {
            settings,
            backend,
            calendar,
        }
    }

    #[cfg(test)]
    pub(crate) fn demo(settings: Settings, options: FakeOptions) -> Context {
        Context::new(
            settings,
            Box::new(FakeBackend::new(options.clone())),
            Box::new(FakeCalendar::new(options)),
        )
    }

    pub(crate) fn from_env() -> Context {
        let settings = Settings::from_env();
        if settings.fake {
            return Context::new(
                settings,
                Box::new(FakeBackend::from_env()),
                Box::new(FakeCalendar::from_env()),
            );
        }
        Context::new(
            settings,
            Box::new(RealBackend::system()),
            Box::new(RealCalendar::system()),
        )
    }
}
