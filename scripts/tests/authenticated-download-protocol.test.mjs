// Cryptographic wire-contract fixture only: no server route, firmware or device.
import {test} from 'node:test';import assert from 'node:assert/strict';
import {generateKeyPairSync,sign,verify,randomBytes,createHash} from 'node:crypto';
const keys=()=>generateKeyPairSync('ed25519');
const digest=b=>createHash('sha256').update(b).digest('hex');
const canonical=body=>Buffer.from(JSON.stringify(body));
const signed=(body,key)=>({body,signature:sign(null,canonical(body),key)});
class Fixture {
 constructor(){this.server=keys();this.device=keys();this.clock=1000;this.keyRevision=1;this.allowed=true;this.used=new Set();this.bytes=Buffer.from('NV17_SYNTHETIC_NOT_FIRMWARE');}
 manifest(){return signed([1,'synthetic-board','1.2.3',this.bytes.length,digest(this.bytes),1],this.server.privateKey);}
 ticket(){return signed([1,randomBytes(32).toString('hex'),'synthetic-device','synthetic-client','synthetic-board',digest(this.bytes),1,this.keyRevision,this.clock+60],this.server.privateKey);}
 proof(ticket,key=this.device.privateKey){return sign(null,canonical([1,ticket.body[1],'GET','/xiaozhi/ota/v2/download/'+digest(this.bytes)]),key);}
 consume(ticket,proof,{method='GET',path='/xiaozhi/ota/v2/download/'+digest(this.bytes),range=false}={}){
  const b=ticket.body;
  if(!Array.isArray(b)||b.length!==9||b[0]!==1||!/^[a-f0-9]{64}$/.test(b[1])||!verify(null,canonical(b),this.server.publicKey,ticket.signature)||
     b[2]!=='synthetic-device'||b[3]!=='synthetic-client'||b[4]!=='synthetic-board'||b[5]!==digest(this.bytes)||b[6]!==1||b[7]!==this.keyRevision||
     !this.allowed||!Number.isSafeInteger(b[8])||b[8]<=this.clock||b[8]>this.clock+60||this.used.has(b[1])||range||method!=='GET'||path!=='/xiaozhi/ota/v2/download/'+b[5]||
     !verify(null,canonical([1,b[1],method,path]),this.device.publicKey,proof))return false;
  this.used.add(b[1]);return true;
 }
}
function verifiedArtifact(manifest,bytes,key){const b=manifest.body;return Array.isArray(b)&&b.length===6&&b[0]===1&&b[1]==='synthetic-board'&&b[2]==='1.2.3'&&Number.isSafeInteger(b[3])&&b[3]===bytes.length&&b[4]===digest(bytes)&&b[5]===1&&verify(null,canonical(b),key,manifest.signature);}
test('trusted manifest and device possession proof permit one synthetic transfer',()=>{const f=new Fixture(),t=f.ticket();assert(verifiedArtifact(f.manifest(),f.bytes,f.server.publicKey));assert(f.consume(t,f.proof(t)));assert(!f.consume(t,f.proof(t)));});
test('claimed MAC/client is insufficient without the enrolled device signing key',()=>{const f=new Fixture(),t=f.ticket();assert(!f.consume(t,f.proof(t,keys().privateKey)));assert(f.consume(t,f.proof(t)),'bad proof did not consume the valid ticket');});
test('all ticket fields and exact download request are signed; tampering and redirects fail',()=>{const f=new Fixture();for(const index of [0,2,3,4,5,6,7,8]){const t=f.ticket(),proof=f.proof(t);t.body[index]=index===8?999:typeof t.body[index]==='number'?44:'forged';assert(!f.consume(t,proof));}const t=f.ticket();assert(!f.consume(t,f.proof(t),{path:'/arbitrary'}));assert(!f.consume(t,f.proof(t),{method:'POST'}));assert(!f.consume(t,f.proof(t),{range:true}));assert(f.consume(t,f.proof(t)));});
test('expiration, revocation and new key revision invalidate old tickets',()=>{const f=new Fixture(),t=f.ticket();f.clock+=60;assert(!f.consume(t,f.proof(t)));const g=new Fixture(),u=g.ticket();g.allowed=false;assert(!g.consume(u,g.proof(u)));g.allowed=true;g.keyRevision++;assert(!g.consume(u,g.proof(u)));});
test('SHA alone does not authenticate a manifest, and altered artifact bytes fail before apply',()=>{const f=new Fixture(),m=f.manifest();assert(!verifiedArtifact(m,Buffer.from('changed'),f.server.publicKey));assert(!verifiedArtifact(m,f.bytes,keys().publicKey));m.body[2]='1.2.4';assert(!verifiedArtifact(m,f.bytes,f.server.publicKey));});
