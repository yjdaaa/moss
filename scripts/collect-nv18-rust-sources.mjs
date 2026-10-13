// Collect corresponding registry archives and actual notice/license bytes.
// Fixed helper Cargo.lock only. No global cache modification or release approval.
import fs from 'node:fs/promises';
import path from 'node:path';
import crypto from 'node:crypto';
import { execFileSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';
const repo=path.resolve(path.dirname(fileURLToPath(import.meta.url)),'..');
const helper=path.join(repo,'apps/moss-voice-local');
const root=path.join(helper,'rust-license-evidence');
await fs.mkdir(root,{recursive:true});
const metadata=JSON.parse(execFileSync('cargo',['metadata','--manifest-path',path.join(helper,'Cargo.toml'),'--locked','--offline','--format-version','1'],{windowsHide:true,maxBuffer:16*1024*1024}).toString());
const lock=await fs.readFile(path.join(helper,'Cargo.lock'),'utf8');
const checksums=new Map();
for(const block of lock.split('[[package]]').slice(1)){
  const name=/^name = "([^"]+)"/m.exec(block)?.[1],version=/^version = "([^"]+)"/m.exec(block)?.[1],checksum=/^checksum = "([a-f0-9]{64})"/m.exec(block)?.[1];
  if(checksum)checksums.set(`${name}@${version}`,checksum);
}
const records=[];let cursor=0;
const packages=metadata.packages.filter(p=>p.source?.startsWith('registry+'));
async function collect(pkg){
  if(!/^[a-zA-Z0-9_-]+$/.test(pkg.name)||!/^[a-zA-Z0-9.+_-]+$/.test(pkg.version))throw Error('Invalid package path');
  const expected=checksums.get(`${pkg.name}@${pkg.version}`);if(!expected)throw Error('Missing locked checksum');
  const file=`${pkg.name}-${pkg.version}.crate`,destination=path.join(root,file);
  let bytes;
  try{bytes=await fs.readFile(destination);}catch(error){
    if(error.code!=='ENOENT')throw error;
    const response=await fetch(`https://static.crates.io/crates/${pkg.name}/${file}`,{signal:AbortSignal.timeout(120000)});
    if(!response.ok)throw Error(`Registry source download failed: ${pkg.name}`);
    bytes=Buffer.from(await response.arrayBuffer());
    if(bytes.length>32*1024*1024)throw Error('Registry source too large');
    if(crypto.createHash('sha256').update(bytes).digest('hex')!==expected)throw Error('Registry checksum mismatch');
    await fs.writeFile(destination,bytes,{flag:'wx'});
  }
  if(crypto.createHash('sha256').update(bytes).digest('hex')!==expected)throw Error('Stored registry source checksum mismatch');
  const prefix=`${pkg.name}-${pkg.version}/`;
  const entries=execFileSync('tar',['-tf',destination],{windowsHide:true,maxBuffer:16*1024*1024}).toString().split(/\r?\n/).filter(e=>e.startsWith(prefix)&&(/^(LICENSE|LICENCE|COPYING|NOTICE|THIRD[_-]PARTY)/i.test(path.posix.basename(e))||/\/LICENSES?\//i.test(e)||path.posix.basename(e)==='AUTHORS')&&!e.endsWith('/'));
  const licenses=[];
  for(let index=0;index<entries.length;index++){
    const entry=entries[index];if(entry.split('/').some(part=>part==='..'||part==='.'))throw Error('Unsafe source entry');
    const text=execFileSync('tar',['-xOf',destination,entry],{windowsHide:true,maxBuffer:2*1024*1024});
    // r-efi includes the complete MIT grant and copyright in AUTHORS;
    // an ordinary author list alone is not a license-text receipt.
    if(path.posix.basename(entry)==='AUTHORS' && !(/LICENSE:/i.test(text.toString())&&/Permission is hereby granted/i.test(text.toString())&&/COPYRIGHT:/i.test(text.toString())))continue;
    const hash=crypto.createHash('sha256').update(text).digest('hex');
    const licenseFile=`${pkg.name}-${pkg.version}-license-${hash.slice(0,16)}.txt`;
    try{await fs.writeFile(path.join(root,licenseFile),text,{flag:'wx'});}catch(error){
      if(error.code!=='EEXIST')throw error;
      if(crypto.createHash('sha256').update(await fs.readFile(path.join(root,licenseFile))).digest('hex')!==hash)throw Error('Existing license changed');
    }
    licenses.push({file:licenseFile,sha256:hash,source_entry:entry});
  }
  records.push({name:pkg.name,version:pkg.version,license_declaration:pkg.license,source_file:file,source_sha256:expected,source_bytes:bytes.length,license_files:licenses,review_status:'pending'});
}
await Promise.all(Array.from({length:5},async()=>{while(cursor<packages.length){const pkg=packages[cursor++];await collect(pkg);}}));
records.sort((a,b)=>`${a.name}@${a.version}`.localeCompare(`${b.name}@${b.version}`));
const missing=records.filter(r=>!r.license_files.length).map(r=>`${r.name}@${r.version}`);
await fs.writeFile(path.join(helper,'rust-source-evidence.json'),JSON.stringify({review_status:'pending',cargo_lock_sha256:crypto.createHash('sha256').update(lock).digest('hex'),registry_packages:records.length,missing_license_texts:missing,packages:records},null,2)+'\n');
console.log(JSON.stringify({registry_archives:records.length,license_texts:records.reduce((n,r)=>n+r.license_files.length,0),missing_license_texts:missing,review_status:'pending'}));
