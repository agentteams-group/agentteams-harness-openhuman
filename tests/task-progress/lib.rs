//! Compile the packaged display adapter with channel interfaces; full SDK tests run in CI.
#![allow(dead_code)]
#[path = "../../patches/task_progress.rs"]
pub(crate) mod progress;
#[allow(unused_imports)]
pub(crate) use openhuman::channels::task_protocol;
mod openhuman {
    pub mod channels {
        pub mod task_protocol {
            pub(crate) fn suppress_progress() -> bool {
                false
            }
        }
        pub mod traits {
            pub struct SendMessage;
            pub struct ChannelMessage;
            #[async_trait::async_trait]
            pub trait Channel: Send + Sync {
                fn name(&self) -> &str;
                async fn send(&self, message: &SendMessage) -> anyhow::Result<()>;
                async fn listen(
                    &self,
                    tx: tokio::sync::mpsc::Sender<ChannelMessage>,
                ) -> anyhow::Result<()>;
                async fn send_progress(
                    &self,
                    room: &str,
                    content: serde_json::Value,
                ) -> anyhow::Result<String>;
            }
        }
    }
    pub mod approval {
        pub mod matrix {
            pub(crate) struct Context {
                pub channel: std::sync::Arc<dyn super::super::channels::traits::Channel>,
                pub room: String,
                pub sender: String,
            }
            tokio::task_local! { pub(crate) static CONTEXT: std::sync::Arc<Context>; }
        }
    }
}
