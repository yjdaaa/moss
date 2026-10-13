// Collect pinned upstream candidates, never claim they were linked or reviewed.
import fs from 'node:fs/promises';import {createReadStream} from 'node:fs';
import path from 'node:path';import crypto from 'node:crypto';import {execFileSync} from 'node:child_process';import {fileURLToPath} from 'node:url';
const repo=path.resolve(path.dirname(fileURLToPath(import.meta.url)),'..');
const native=path.join(repo,'apps/moss-voice-local/native-license-evidence');
const root=path.join(native,'ort-dependencies');await fs.mkdir(root,{recursive:true});
const archive=path.join(native,'ort.tar.gz');const digest=(b,kind='sha256')=>crypto.createHash(kind).update(b).digest('hex');
const h=crypto.createHash('sha256');for await(const chunk of createReadStream(archive))h.update(chunk);
const safeError=e=>{const code=String(e.code||e.message||'collection_failed');return /^[a-zA-Z0-9_]+$/.test(code)?code:'archive_processing_failed';};
const rootHash=h.digest('hex');if(rootHash!=='a5003dfd85d66a4d45bd2757c7f813f9e835142e9a936ac04877c788d6afe938')throw Error('Pinned ORT root archive mismatch');
const extract=p=>execFileSync('tar',['-xOf',archive,'onnxruntime-1.28.2/'+p],{windowsHide:true,maxBuffer:2*1024*1024});
const deps=extract('cmake/deps.txt'),modules=extract('.gitmodules');
const records=deps.toString().split(/\r?\n/).filter(s=>s.trim()&&!s.trim().startsWith('#')).map(s=>{
 const [name,url,sha1,...extra]=s.split(';');if(extra.length||!/^[a-z0-9_-]+$/.test(name)||!/^[a-f0-9]{40}$/.test(sha1))throw Error('Pinned dependency format changed');
 const u=new URL(url);if(u.protocol!=='https:'||!['github.com','codeload.github.com'].includes(u.hostname)||u.username||u.password)throw Error('Unexpected public dependency source');
 return {name,source_url:url,upstream_sha1:sha1,declared_by:'onnxruntime-1.28.2/cmake/deps.txt',role:name.startsWith('protoc_')?'host_tool_binary_not_corresponding_source':'optional_source_candidate',linked:'unverified',review_status:'pending'};
});
const names=new Set(records.map(r=>r.name));if(names.size!==records.length)throw Error('Duplicate dependency names');
let total=0;const perFile=512*1024*1024,budget=1536*1024*1024;
async function fileHashes(file){const stat=await fs.lstat(file);if(!stat.isFile()||stat.isSymbolicLink()||stat.size>perFile)throw Error('invalid_source_file');const sha1=crypto.createHash('sha1'),sha256=crypto.createHash('sha256');for await(const chunk of createReadStream(file)){sha1.update(chunk);sha256.update(chunk);}return {bytes:stat.size,sha1:sha1.digest('hex'),sha256:sha256.digest('hex')};}
async function download(record,destination){
 const temporary=destination+'.'+crypto.randomUUID()+'.download';let handle=null;let reader=null;
 try{const response=await fetch(record.source_url,{signal:AbortSignal.timeout(180000)});if(!response.ok)throw Error('http_'+response.status);if(Number(response.headers.get('content-length'))>perFile)throw Error('source_file_budget');
  reader=response.body.getReader();handle=await fs.open(temporary,'wx');let length=0;
  while(true){const {done,value}=await reader.read();if(done)break;length+=value.length;if(length>perFile||total+value.length>budget)throw Error('source_budget');total+=value.length;
   let offset=0;while(offset<value.length){const written=await handle.write(value,offset,value.length-offset,null);if(!written.bytesWritten)throw Error('source_write_failed');offset+=written.bytesWritten;}
  }
  await handle.close();handle=null;const hashes=await fileHashes(temporary);if(record.upstream_sha1&&hashes.sha1!==record.upstream_sha1)throw Error('upstream_sha1_mismatch');
  try{await fs.link(temporary,destination);}catch(e){if(e.code!=='EEXIST')throw e;const existing=await fileHashes(destination);if(existing.sha256!==hashes.sha256)throw Error('existing_source_changed');}return hashes;
 }finally{if(reader)await reader.cancel().catch(()=>{});if(handle)await handle.close();await fs.unlink(temporary).catch(e=>{if(e.code!=='ENOENT')throw e;});}
}
async function collect(record){
 if(record.role==='host_tool_binary_not_corresponding_source'){record.collection='not_collected_binary';return;}
 const filename=record.name+(record.git_commit?'-'+record.git_commit.slice(0,12):'')+(record.source_url.includes('.zip')?'.zip':'.tar.gz');const destination=path.join(root,filename);
 try{
  let hashes;try{hashes=await fileHashes(destination);if(total+hashes.bytes>budget)throw Error('source_total_budget');total+=hashes.bytes;}catch(e){if(e.code!=='ENOENT')throw e;hashes=await download(record,destination);}
  if(record.upstream_sha1&&hashes.sha1!==record.upstream_sha1)throw Error('upstream_sha1_mismatch');
  record.source_file='ort-dependencies/'+filename;record.source_bytes=hashes.bytes;record.source_sha256=hashes.sha256;record.collection='collected_candidate';
  const entries=execFileSync('tar',['-tf',destination],{windowsHide:true,maxBuffer:16*1024*1024}).toString().split(/\r?\n/).filter(e=>!e.endsWith('/')&&(/^(LICENSE|LICENCE|COPYING|NOTICE|THIRD[_-]PARTY)/i.test(path.posix.basename(e))||/\/LICENSES?\//i.test(e)));
  record.license_files=[];
  for(const entry of entries){if(entry.startsWith('/')||entry.split('/').some(v=>v==='..'||v==='.'))throw Error('unsafe_archive_entry');
   const text=execFileSync('tar',['-xOf',destination,entry],{windowsHide:true,maxBuffer:2*1024*1024});const hash=digest(text),name=record.name+'-license-'+hash.slice(0,16)+'.txt';
   try{await fs.writeFile(path.join(root,name),text,{flag:'wx'});}catch(e){if(e.code!=='EEXIST')throw e;if(digest(await fs.readFile(path.join(root,name)))!==hash)throw Error('stored_license_mismatch');}
   record.license_files.push({file:'ort-dependencies/'+name,sha256:hash,source_entry:entry});
  }
 }catch(e){record.collection='incomplete';record.error=safeError(e);}
 console.log(JSON.stringify({dependency:record.name,collection:record.collection,bytes:record.source_bytes??0}));
}
let cursor=0;await Promise.all(Array.from({length:3},async()=>{while(cursor<records.length)await collect(records[cursor++]);}));
// Resolve gitlinks at the exact release tree. Bind its two declaration blobs
// to the measured archive; this still does not prove the prebuilt build tree.
const gitlinks=[];let gitlinkError=null;
try{
 const response=await fetch('https://api.github.com/repos/microsoft/onnxruntime/git/trees/v1.28.2?recursive=1',{headers:{Accept:'application/vnd.github+json'},signal:AbortSignal.timeout(30000)});if(!response.ok)throw Error('github_tree_'+response.status);
 const tree=await response.json();if(tree.truncated)throw Error('truncated_release_tree');
 const blobHash=b=>digest(Buffer.concat([Buffer.from('blob '+b.length+'\0'),b]),'sha1');
 for(const [name,bytes] of [['cmake/deps.txt',deps],['.gitmodules',modules]])if(tree.tree.find(e=>e.path===name)?.sha!==blobHash(bytes))throw Error('release_declaration_blob_mismatch');
 for(const match of modules.toString().matchAll(/path = ([^\r\n]+)\r?\n\s*url = https:\/\/github.com\/([^\s]+?)\.git/g)){
  const entry=tree.tree.find(e=>e.path===match[1]&&e.mode==='160000');if(!entry||!/^[a-f0-9]{40}$/.test(entry.sha)||!/^[-a-zA-Z0-9_]+\/[-a-zA-Z0-9_]+$/.test(match[2]))throw Error('gitlink_not_pinned');
  const record={name:'gitlink-'+path.posix.basename(entry.path),source_url:'https://github.com/'+match[2]+'/archive/'+entry.sha+'.tar.gz',git_commit:entry.sha,declared_by:'onnxruntime-v1.28.2 git tree',declaration_binding:'deps.txt/.gitmodules Git blob hashes match pinned tar; whole tree/prebuilt correspondence unreviewed',role:'recursive_source_candidate',linked:'unverified',review_status:'pending'};
  await collect(record);gitlinks.push(record);
 }
}catch(e){gitlinkError=safeError(e);}
const all=[...records,...gitlinks],missing=all.filter(r=>r.collection==='incomplete').map(r=>r.name);
const report={schema:1,root_archive_sha256:rootHash,review_status:'pending',reviewed_by:'',release_ready:false,source_budget_bytes:budget,collected_bytes:all.reduce((n,r)=>n+(r.source_bytes??0),0),declared_dependencies:records.length,skipped_host_binaries:records.filter(r=>r.collection==='not_collected_binary').length,gitlink_resolution_error:gitlinkError,uncollected_sources:missing,limitations:['candidate collection is not proof of linked dependencies','recursive dependencies inside candidate archives not proven complete','prebuilt CMake/build provenance and independent review remain missing'],dependencies:all};
await fs.writeFile(path.join(native,'ort-dependency-evidence.json'),JSON.stringify(report,null,2)+'\n');
console.log(JSON.stringify({declared_dependencies:records.length,collected_sources:all.filter(r=>r.collection==='collected_candidate').length,gitlinks:gitlinks.length,bytes:total,missing,gitlink_error:gitlinkError,release_ready:false}));
if(missing.length||gitlinkError)process.exitCode=2;
