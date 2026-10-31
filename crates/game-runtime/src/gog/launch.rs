//! A GOG launch: the installed build's primary play task, natively or in its
//! Wine prefix. GOG's builds need no account session to start.
use super::*;
use crate::launch_shell::{Launchable as _, Request};
use crate::supervisor::{
    Target,
    architecture::Architecture,
    session::{self as lifecycle, LaunchCloud, Opening, Prepared, Session},
};
use std::{
    io::{Read, Write},
    sync::Arc,
};

pub(crate) struct Launch {
    request: Request<Gog>,
}

impl lifecycle::Launch for Launch {
    type P = Gog;
    type Host = ();
    type Cloud = Box<dyn LaunchCloud<Error>>;

    fn wine(&self) -> Option<crate::wine::Config> {
        self.request.wine.clone()
    }

    fn open(input: &mut dyn Read) -> Result<(Opening<Gog>, Self)> {
        let (opening, request) = Request::<Gog>::open(input)?;
        Ok((opening, Self { request }))
    }

    async fn cloud(&mut self, _: &Arc<Session<Gog>>) -> Result<Option<Self::Cloud>> {
        Ok(None)
    }

    async fn target(&mut self, _: &Arc<Session<Gog>>) -> Result<(Target, Option<Architecture>)> {
        let installed = Gog::installed(&self.request.config)?;
        let started = self.request.target(&installed.target, None)?;
        Ok((started.target, Some(started.architecture)))
    }

    async fn prepare_host(
        &mut self,
        _: &Arc<Session<Gog>>,
        target: &Target,
        _: Option<Architecture>,
        _: Option<&Self::Cloud>,
    ) -> Result<Prepared<()>> {
        Ok(self.request.prepared((), target))
    }

    fn running(&self, _: &Arc<()>, _: u32) -> Box<dyn Fn(bool) + Send + Sync> {
        Box::new(|_| {})
    }

    fn stopped(&self, _: &(), _: Option<i32>) {}

    /// No Cloud sync settles a GOG game's saves.
    fn terminal(record: &mut Snapshot) {
        record.cloud.allowed_choices.clear();
        record.cloud.phase = crate::cloud::Phase::Unsupported;
    }
}

/// The `obelisk-steam-host gog-supervise` entry point.
pub fn serve(input: impl Read, output: impl Write, inspect: lifecycle::Inspect) -> Result<()> {
    lifecycle::serve::<Launch>(input, output, inspect)
}
