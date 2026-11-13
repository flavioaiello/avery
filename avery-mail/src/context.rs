use crate::backend::Backend;
use crate::calendar::{CalendarBackend, FakeCalendar};
use crate::config::Settings;
use crate::fake::FakeBackend;
#[cfg(test)]
use crate::fake::FakeOptions;
use crate::provider::{Calendars, Linker, Mail};

pub(crate) struct Context {
    pub(crate) settings: Settings,
    pub(crate) backend: Box<dyn Backend>,
    pub(crate) calendar: Box<dyn CalendarBackend>,
    pub(crate) linker: Linker,
}

impl Context {
    pub(crate) fn new(
        settings: Settings,
        backend: Box<dyn Backend>,
        calendar: Box<dyn CalendarBackend>,
        linker: Linker,
    ) -> Context {
        Context {
            settings,
            backend,
            calendar,
            linker,
        }
    }

    #[cfg(test)]
    pub(crate) fn demo(settings: Settings, options: FakeOptions) -> Context {
        Context::new(
            settings,
            Box::new(FakeBackend::new(options.clone())),
            Box::new(FakeCalendar::new(options)),
            Linker::inert(),
        )
    }

    pub(crate) fn from_env() -> Context {
        let settings = Settings::from_env();
        if settings.fake {
            return Context::new(
                settings,
                Box::new(FakeBackend::from_env()),
                Box::new(FakeCalendar::from_env()),
                Linker::inert(),
            );
        }
        Context::new(
            settings,
            Box::new(Mail::system()),
            Box::new(Calendars::system()),
            Linker::system(),
        )
    }
}
