//! NV13: AgentRun persistence is intentionally separate from external MCP and laser jobs.
//! The full task remains only in the hidden approval.request; this table stores
//! bounded, already redacted output and links each run to one consumed approval.
use crate::db::{RuntimeDb,SchemaError};
use rusqlite::{OptionalExtension,params};
use serde::{Deserialize,Serialize};

#[derive(Debug,Clone,PartialEq,Serialize,Deserialize)]
pub struct AgentRunRecord {
    pub id:String,
    pub approval_id:String,
    pub provider:String,
    pub workspace:String,
    pub task_digest:String,
    pub config_revision:String,
    pub status:String,
    pub output:String,
    pub truncated:bool,
    pub started_at:f64,
    pub finished_at:Option<f64>,
    pub error_code:Option<String>,
}
const COLUMNS:&str="id,approval_id,provider,workspace,task_digest,config_revision,status,output,truncated,started_at,finished_at,error_code";
fn from_row(row:&rusqlite::Row<'_>)->rusqlite::Result<AgentRunRecord>{
 Ok(AgentRunRecord{
  id:row.get(0)?,approval_id:row.get(1)?,provider:row.get(2)?,
  workspace:row.get(3)?,task_digest:row.get(4)?,config_revision:row.get(5)?,
  status:row.get(6)?,output:row.get(7)?,truncated:row.get::<_,i64>(8)?!=0,
  started_at:row.get(9)?,finished_at:row.get(10)?,error_code:row.get(11)?,
 })
}
impl RuntimeDb {
    /// Insert within the SAME SQLite transaction as one-time approval consumption.
    pub fn insert_agent_run(&self,run:&AgentRunRecord)->Result<(),SchemaError>{
        if run.id.is_empty()||run.approval_id.is_empty()
           ||run.task_digest.len()!=64||run.config_revision.len()!=64
           ||run.output.len()>256*1024||run.status!="running"
           ||!["codex","claude_code"].contains(&run.provider.as_str()){
            return Err(SchemaError::InvalidRecord("AgentRun 记录无效".into()));
        }
        self.connection.execute(
         "INSERT INTO voice_agent_runs(id,approval_id,provider,workspace,task_digest,config_revision,status,output,truncated,started_at,finished_at,error_code)
          VALUES(?1,?2,?3,?4,?5,?6,'running','',0,?7,NULL,NULL)",
         params![run.id,run.approval_id,run.provider,run.workspace,run.task_digest,run.config_revision,run.started_at],
        )?;
        // Only prune TERMINAL runs; active ones must never disappear.
        self.connection.execute(
          "DELETE FROM voice_agent_runs WHERE status<>'running' AND id NOT IN
           (SELECT id FROM voice_agent_runs ORDER BY started_at DESC,id DESC LIMIT 50)",[]
        )?;
        Ok(())
    }
    pub fn find_agent_run(&self,id:&str)->Result<Option<AgentRunRecord>,SchemaError>{
        self.connection.query_row(
          &format!("SELECT {COLUMNS} FROM voice_agent_runs WHERE id=?1"),[id],from_row
        ).optional().map_err(SchemaError::from)
    }
    pub fn finish_agent_run(&self,id:&str,status:&str,output:&str,truncated:bool,error_code:Option<&str>,now:f64)->Result<bool,SchemaError>{
        if !matches!(status,"completed"|"failed"|"cancelled"|"timed_out")
           ||output.len()>256*1024||error_code.is_some_and(|s|s.len()>128){
            return Err(SchemaError::InvalidRecord("AgentRun 终态或输出长度无效".into()));
        }
        Ok(self.connection.execute(
          "UPDATE voice_agent_runs SET status=?2,output=?3,truncated=?4,finished_at=?5,error_code=?6
           WHERE id=?1 AND status='running'",
          params![id,status,output,i64::from(truncated),now,error_code],
        )?==1)
    }
    pub fn agent_runs(&self,limit:usize)->Result<Vec<AgentRunRecord>,SchemaError>{
        let mut statement=self.connection.prepare(&format!(
         "SELECT {COLUMNS} FROM voice_agent_runs ORDER BY started_at DESC,id DESC LIMIT ?1"))?;
        let rows=statement.query_map([limit.min(50) as i64],from_row)?;
        Ok(rows.collect::<Result<Vec<_>,_>>()?)
    }
    pub fn recover_agent_runs(&self,now:f64)->Result<usize,SchemaError>{
        Ok(self.connection.execute(
          "UPDATE voice_agent_runs SET status='result_unknown',finished_at=?1,error_code='result_unknown'
           WHERE status='running'",[now])?)
    }
}

#[cfg(test)]
mod tests{
 use super::*;
 fn sample(idx:u32)->AgentRunRecord{
  AgentRunRecord{
   id:format!("run-{idx}"),approval_id:format!("approval-{idx}"),provider:"codex".into(),
   workspace:"fake-workspace".into(),task_digest:"a".repeat(64),config_revision:"b".repeat(64),
   status:"running".into(),output:String::new(),truncated:false,started_at:idx as f64,
   finished_at:None,error_code:None,
  }
 }
 #[test]fn bounded_history_duplicate_approval_and_restart_recovery(){
   let db=RuntimeDb::open_memory().unwrap();
   assert_eq!(db.schema_version().unwrap(),super::super::db::SCHEMA_VERSION);
   for index in 0..55{
     let entry=sample(index);db.insert_agent_run(&entry).unwrap();
     assert!(!db.insert_agent_run(&entry).is_ok(),"同一审批不得重复执行");
     assert!(db.finish_agent_run(&entry.id,"completed","ok",false,None,100.0).unwrap());
     assert!(!db.finish_agent_run(&entry.id,"completed","repeat",false,None,101.0).unwrap());
   }
   assert_eq!(db.agent_runs(100).unwrap().len(),50);
   assert!(db.find_agent_run("run-0").unwrap().is_none());
   let active=sample(100);db.insert_agent_run(&active).unwrap();
   assert_eq!(db.recover_agent_runs(200.0).unwrap(),1);
   assert_eq!(db.find_agent_run("run-100").unwrap().unwrap().status,"result_unknown");
   assert!(!db.finish_agent_run("run-100","completed","late",false,None,201.0).unwrap());
   assert!(db.finish_agent_run("run-54","completed",&"x".repeat(256*1024+1),false,None,210.0).is_err());
 }
 #[test]fn schema_v8_migration_is_atomic(){
   let db=RuntimeDb::open_memory().unwrap();
   db.connection.execute_batch(
    "DROP TABLE voice_agent_runs; DELETE FROM schema_version WHERE version>=8; CREATE TABLE voice_agent_runs(unexpected TEXT);"
   ).unwrap();
   assert_eq!(db.schema_version().unwrap(),7);
   assert!(db.migrate().is_err());
   assert_eq!(db.schema_version().unwrap(),7);
   db.connection.execute_batch("DROP TABLE voice_agent_runs;").unwrap();
   db.migrate().unwrap();
   assert_eq!(db.schema_version().unwrap(),super::super::db::SCHEMA_VERSION);
 }
}
