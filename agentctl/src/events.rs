use std::fs::OpenOptions;
use std::io::{BufWriter, Write};
use std::path::PathBuf;

use anyhow::Result;
use serde::Serialize;

use crate::run_id;

pub struct EventWriter {
    run_id: String,
    seq: u64,
    writer: BufWriter<std::fs::File>,
}

#[derive(Serialize)]
struct Event<'a, T: Serialize> {
    v: &'a str,
    ts: &'a str,
    run_id: &'a str,
    seq: u64,
    #[serde(rename = "type")]
    event_type: &'a str,
    data: &'a T,
}

impl EventWriter {
    pub fn new(run_id: String, path: PathBuf) -> Result<Self> {
        let file = OpenOptions::new().create(true).append(true).open(path)?;
        Ok(Self {
            run_id,
            seq: 0,
            writer: BufWriter::new(file),
        })
    }

    pub fn emit<T: Serialize>(&mut self, event_type: &'static str, data: &T) -> Result<()> {
        self.seq += 1;
        let ts = run_id::timestamp();
        let event = Event {
            v: "runfmt/0.1",
            ts: &ts,
            run_id: &self.run_id,
            seq: self.seq,
            event_type,
            data,
        };
        serde_json::to_writer(&mut self.writer, &event)?;
        self.writer.write_all(b"\n")?;
        self.writer.flush()?;
        Ok(())
    }
}
