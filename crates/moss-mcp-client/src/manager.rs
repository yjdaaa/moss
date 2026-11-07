//! Bounded, independent connection workers. No tool call is retried after reconnect.
use crate::*;
use serde::Serialize;
use serde_json::Value;
use std::collections::{BTreeMap, HashSet};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;
use tokio::sync::{mpsc, oneshot, watch};

/// Implemented only by trusted host code. Never built from a model-supplied approval flag.
pub trait DispatchGuard: Send + Sync {
    fn authorize(&self, intent: &CallIntent, permission: Permission) -> Result<(), ClientError>;
    /// A cheap live permission fence, checked immediately before tools/call.
    fn is_valid(&self) -> bool;
    /// Called even when a caller has stopped waiting. The host persists the final outcome.
    fn completed(
        &self,
        intent: &CallIntent,
        result: &Result<Value, ClientError>,
    ) -> Result<(), ClientError>;
}

#[derive(Clone, Serialize)]
pub struct ManagedTool {
    pub name: String,
    pub original: String,
    pub description: String,
    pub parameters: Value,
    pub permission: Permission,
    pub stats: CallStats,
    pub blocked_reason: Option<String>,
}
#[derive(Clone, Serialize)]
pub struct ConnectionView {
    pub name: String,
    pub state: String,
    pub last_error: Option<String>,
    pub connections: u64,
    pub retry_seconds: Option<u64>,
    pub tools: Vec<ManagedTool>,
}
#[derive(Clone)]
pub struct PreparedCall {
    pub intent: CallIntent,
    pub permission: Permission,
    generation: Arc<AtomicBool>,
}

enum Command {
    Prepare {
        alias: String,
        arguments: Value,
        reply: oneshot::Sender<Result<(CallIntent, Permission), ClientError>>,
    },
    Execute {
        prepared: PreparedCall,
        guard: Arc<dyn DispatchGuard>,
        reply: oneshot::Sender<Result<Value, ClientError>>,
    },
}
struct Entry {
    active: Arc<AtomicBool>,
    stop: watch::Sender<bool>,
    sender: mpsc::Sender<Command>,
    view: Arc<Mutex<ConnectionView>>,
}
impl Drop for Entry {
    fn drop(&mut self) {
        self.active.store(false, Ordering::Release);
        self.stop.send_replace(true);
    }
}

pub struct Manager {
    runtime: tokio::runtime::Handle,
    entries: Mutex<BTreeMap<String, Entry>>,
    reserved: HashSet<String>,
    heartbeat: Duration,
}
impl Manager {
    pub fn new(runtime: tokio::runtime::Handle, reserved: HashSet<String>) -> Self {
        Self {
            runtime,
            entries: Mutex::default(),
            reserved,
            heartbeat: HEARTBEAT_INTERVAL,
        }
    }
    /// Tests inject shorter clocks; production always uses HEARTBEAT_INTERVAL.
    pub fn with_heartbeat(mut self, duration: Duration) -> Self {
        self.heartbeat = duration.max(Duration::from_millis(10));
        self
    }

    /// Pause immediately without losing this run's counters. Configuration save calls this before persistence.
    pub fn suspend(&self, name: &str) {
        if let Ok(entries) = self.entries.lock()
            && let Some(entry) = entries.get(name)
        {
            entry.active.store(false, Ordering::Release);
            entry.stop.send_replace(true);
            if let Ok(mut view) = entry.view.lock() {
                view.state = "disabled".into();
            }
        }
    }
    pub fn remove(&self, name: &str) {
        if let Ok(mut entries) = self.entries.lock() {
            entries.remove(name);
        }
    }
    pub fn shutdown(&self) {
        if let Ok(mut entries) = self.entries.lock() {
            entries.clear();
        }
    }

    pub fn configure(
        &self,
        config: ServerConfig,
        secrets: BTreeMap<String, String>,
    ) -> Result<(), ClientError> {
        config.validate()?;
        let mut entries = self.entries.lock().map_err(|_| ClientError::Unavailable)?;
        if !entries.contains_key(&config.name) && entries.len() >= MAX_SERVERS {
            return Err(ClientError::InvalidConfig);
        }
        if entries
            .keys()
            .any(|name| name != &config.name && name.eq_ignore_ascii_case(&config.name))
        {
            return Err(ClientError::DuplicateName);
        }
        let previous = entries
            .remove(&config.name)
            .and_then(|entry| entry.view.lock().ok().map(|view| view.clone()));
        let mut view = previous.unwrap_or_else(|| ConnectionView {
            name: config.name.clone(),
            state: String::new(),
            last_error: None,
            connections: 0,
            retry_seconds: None,
            tools: vec![],
        });
        view.state = if config.enabled {
            "connecting"
        } else {
            "disabled"
        }
        .into();
        view.last_error = None;
        for tool in &mut view.tools {
            tool.permission = config.permission(&tool.original);
        }
        let view = Arc::new(Mutex::new(view));
        let active = Arc::new(AtomicBool::new(config.enabled));
        let (stop, rx_stop) = watch::channel(false);
        let (sender, receiver) = mpsc::channel(16);
        entries.insert(
            config.name.clone(),
            Entry {
                active: active.clone(),
                stop,
                sender,
                view: view.clone(),
            },
        );
        if config.enabled {
            self.runtime.spawn(worker(
                config,
                secrets,
                active,
                rx_stop,
                receiver,
                view,
                self.heartbeat,
            ));
        }
        Ok(())
    }

    pub fn snapshot(&self) -> Vec<ConnectionView> {
        let Ok(entries) = self.entries.lock() else {
            return vec![];
        };
        let mut views: Vec<_> = entries
            .values()
            .filter_map(|entry| {
                entry.view.lock().ok().map(|v| {
                    let mut view = v.clone();
                    if !entry.active.load(Ordering::Acquire) {
                        view.state = "disabled".into();
                    }
                    view
                })
            })
            .collect();
        let mut names = self.reserved.clone();
        let mut count = 0;
        for view in &mut views {
            for tool in &mut view.tools {
                tool.blocked_reason = if tool.permission == Permission::Disabled {
                    Some("工具已禁用".into())
                } else if view.state != "connected" {
                    Some("服务未连接".into())
                } else if !names.insert(tool.name.clone()) {
                    Some("工具名称冲突".into())
                } else if count >= MAX_TOOLS {
                    Some("外部工具总数超过 64，请禁用不需要的工具".into())
                } else {
                    count += 1;
                    None
                };
            }
        }
        views
    }
    pub fn tools(&self) -> Vec<ToolDefinition> {
        self.snapshot()
            .into_iter()
            .flat_map(|view| view.tools)
            .filter(|tool| tool.blocked_reason.is_none())
            .map(|tool| ToolDefinition {
                name: tool.name,
                description: tool.description,
                parameters: tool.parameters,
            })
            .collect()
    }
    fn sender(&self, alias: &str) -> Result<(mpsc::Sender<Command>, Arc<AtomicBool>), ClientError> {
        // Enforce the same global budget/collision rules at invocation, not only in the model list.
        if !self.tools().iter().any(|tool| tool.name == alias) {
            return Err(ClientError::Disabled);
        }
        let (name, _) = alias.split_once("__").ok_or(ClientError::InvalidName)?;
        let entries = self.entries.lock().map_err(|_| ClientError::Unavailable)?;
        let entry = entries.get(name).ok_or(ClientError::Unavailable)?;
        if !entry.active.load(Ordering::Acquire) {
            return Err(ClientError::Disabled);
        }
        Ok((entry.sender.clone(), entry.active.clone()))
    }
    pub async fn prepare(
        &self,
        alias: &str,
        arguments: Value,
    ) -> Result<PreparedCall, ClientError> {
        let (sender, generation) = self.sender(alias)?;
        let (reply, receiver) = oneshot::channel();
        let result = tokio::time::timeout(CALL_TIMEOUT, async {
            sender
                .send(Command::Prepare {
                    alias: alias.into(),
                    arguments,
                    reply,
                })
                .await
                .map_err(|_| ClientError::Unavailable)?;
            receiver.await.map_err(|_| ClientError::Unavailable)?
        })
        .await
        .map_err(|_| ClientError::Unavailable)??;
        if !generation.load(Ordering::Acquire) {
            return Err(ClientError::Disabled);
        }
        Ok(PreparedCall {
            intent: result.0,
            permission: result.1,
            generation,
        })
    }
    pub async fn execute(
        &self,
        prepared: PreparedCall,
        guard: Arc<dyn DispatchGuard>,
    ) -> Result<Value, ClientError> {
        let alias = tool_alias(&prepared.intent.server, &prepared.intent.tool)?;
        let (sender, generation) = self.sender(&alias)?;
        if !Arc::ptr_eq(&generation, &prepared.generation) || !generation.load(Ordering::Acquire) {
            return Err(ClientError::ApprovalFailed);
        }
        let (reply, receiver) = oneshot::channel();
        tokio::time::timeout(CALL_TIMEOUT + Duration::from_secs(5), async {
            sender
                .send(Command::Execute {
                    prepared,
                    guard,
                    reply,
                })
                .await
                .map_err(|_| ClientError::Unavailable)?;
            receiver.await.map_err(|_| ClientError::OutcomeUnknown)?
        })
        .await
        .map_err(|_| ClientError::OutcomeUnknown)?
    }
}

fn publish(
    view: &Mutex<ConnectionView>,
    state: &str,
    error: Option<ClientError>,
    retry: Option<Duration>,
    client: Option<&Client>,
) {
    if let Ok(mut view) = view.lock() {
        view.state = state.into();
        view.last_error = error.map(|e| e.to_string());
        view.retry_seconds = retry.map(|d| d.as_secs());
        if let Some(client) = client {
            view.tools = client
                .catalog()
                .into_iter()
                .map(|(original, tool, permission)| ManagedTool {
                    stats: client.stats().get(&tool.name).cloned().unwrap_or_default(),
                    original,
                    name: tool.name,
                    description: tool.description,
                    parameters: tool.parameters,
                    permission,
                    blocked_reason: None,
                })
                .collect();
        }
    }
}

async fn worker(
    config: ServerConfig,
    secrets: BTreeMap<String, String>,
    active: Arc<AtomicBool>,
    mut stop: watch::Receiver<bool>,
    mut commands: mpsc::Receiver<Command>,
    view: Arc<Mutex<ConnectionView>>,
    heartbeat: Duration,
) {
    let mut backoff = ReconnectBackoff::default();
    let mut stats: BTreeMap<String, CallStats> = view
        .lock()
        .map(|v| {
            v.tools
                .iter()
                .map(|t| (t.name.clone(), t.stats.clone()))
                .collect()
        })
        .unwrap_or_default();
    while active.load(Ordering::Acquire) {
        let connected = tokio::select! {
            biased;
            _ = stop.changed() => break,
            result = Client::connect(config.clone(),&secrets,Arc::new(RequireApproval)) => result,
        };
        let mut client = match connected {
            Ok(mut client) => {
                client.restore_stats(std::mem::take(&mut stats));
                if let Ok(mut v) = view.lock() {
                    v.connections = v.connections.saturating_add(1);
                }
                publish(&view, "connected", None, None, Some(&client));
                client
            }
            Err(error) => {
                let delay = backoff.next_delay();
                publish(&view, "reconnecting", Some(error), Some(delay), None);
                tokio::select! { _=stop.changed()=>break, _=tokio::time::sleep(delay)=>{} }
                continue;
            }
        };
        let mut clock =
            tokio::time::interval_at(tokio::time::Instant::now() + heartbeat, heartbeat);
        clock.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        while active.load(Ordering::Acquire) && client.is_connected() {
            tokio::select! {
                biased;
                _=stop.changed()=>break,
                command=commands.recv()=>match command {
                    Some(Command::Prepare { alias,arguments,reply }) => { if !reply.is_closed() { let _=reply.send(client.prepare(&alias,arguments)); } }
                    Some(Command::Execute { prepared,guard,reply }) => {
                        if reply.is_closed() { continue; }
                        let alias=tool_alias(&prepared.intent.server,&prepared.intent.tool).unwrap_or_default();
                        let current=client.prepare(&alias,Value::Object(prepared.intent.arguments.clone()));
                        if !active.load(Ordering::Acquire) || !guard.is_valid() || !matches!(&current,Ok((intent,permission)) if intent.digest==prepared.intent.digest && *permission==prepared.permission) {
                            let _=reply.send(Err(ClientError::ApprovalFailed)); continue;
                        }
                        let auth_guard=guard.clone(); let intent=prepared.intent.clone(); let permission=prepared.permission;
                        let authorized=tokio::task::spawn_blocking(move||auth_guard.authorize(&intent,permission)).await.unwrap_or(Err(ClientError::ApprovalFailed));
                        if let Err(error)=authorized { let _=reply.send(Err(error)); continue; }
                        let result=if !active.load(Ordering::Acquire) || !guard.is_valid() || reply.is_closed() { Err(ClientError::Disabled) } else {
                            tokio::select! { biased; _=stop.changed()=>Err(ClientError::OutcomeUnknown), result=client.execute_prepared(&prepared.intent)=>result }
                        };
                        let done_result=result.clone(); let done_intent=prepared.intent; let done_guard=guard;
                        let recorded=tokio::task::spawn_blocking(move||done_guard.completed(&done_intent,&done_result)).await;
                        let result=if matches!(recorded,Ok(Ok(()))) {result} else {Err(ClientError::OutcomeNotRecorded)};
                        publish(&view,if client.is_connected(){"connected"}else{"reconnecting"},result.as_ref().err().copied(),None,Some(&client));
                        let _=reply.send(result);
                    }
                    None=>break,
                },
                _=clock.tick()=>{
                    let health=tokio::select!{_ =stop.changed()=>break,result=async {client.heartbeat().await?; client.refresh_tools().await}=>result};
                    if let Err(error)=health { publish(&view,"reconnecting",Some(error),None,Some(&client)); break; }
                    backoff.reset_after_healthy_connection(); publish(&view,"connected",None,None,Some(&client));
                }
            }
        }
        stats = client.stats().clone();
        client.disconnect().await;
        if !active.load(Ordering::Acquire) {
            break;
        }
        let delay = backoff.next_delay();
        publish(
            &view,
            "reconnecting",
            Some(ClientError::Unavailable),
            Some(delay),
            None,
        );
        tokio::select! {_=stop.changed()=>break,_=tokio::time::sleep(delay)=>{}}
    }
    publish(&view, "disabled", None, None, None);
}
