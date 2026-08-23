mod batch;
mod command;
mod health;
mod session;

use serde_json::Value;

pub(super) fn input_schema(name: &str) -> Option<Value> {
    health::input_schema(name)
        .or_else(|| batch::input_schema(name))
        .or_else(|| command::input_schema(name))
        .or_else(|| session::input_schema(name))
}
