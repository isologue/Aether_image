import sys,json,time,urllib.request,urllib.error,socket,gzip
from pathlib import Path
sys.path.insert(0,str(Path(r"G:\Demo\Aether\output\online-audit-20261023")))
from remote import connect,sql
c=connect()
try:
 code=r"""
import subprocess,json,urllib.request
info=json.loads(subprocess.check_output(['docker','inspect','aether-app']))[0]
e={k:v for k,v in (s.split('=',1) for s in info['Config']['Env'] if '=' in s)}
base='http://127.0.0.1:'+e.get('APP_PORT','8084')
op=urllib.request.build_opener(urllib.request.ProxyHandler({}))
data=json.dumps({'email':e.get('ADMIN_USERNAME') or e.get('ADMIN_EMAIL') or 'admin','password':e.get('ADMIN_PASSWORD','')}).encode()
h={'Content-Type':'application/json','X-Client-Device-Id':'aether-wan-body-audit-20261024'}
with op.open(urllib.request.Request(base+'/api/auth/login',data=data,headers=h),timeout=15) as r:
 print(json.dumps({'token':json.load(r)['access_token'],'port':e.get('APP_PORT','8084')}))
"""
 stdin,out,err=c.exec_command('python3 -',timeout=25)
 stdin.write(code);stdin.channel.shutdown_write()
 auth=json.loads(out.read().decode()); token=auth['token']
 base='http://'+c.get_transport().getpeername()[0]+':'+str(auth['port'])
 normal=json.loads(sql(c,"SELECT json_build_object('id',u.id,'request_id',u.request_id) FROM usage u WHERE u.created_at >= '2026-10-24 11:09:00+00'::timestamptz AND u.created_at < '2026-10-24 11:13:00+00'::timestamptz AND COALESCE(u.request_metadata->>'response_outcome','') <> 'content_refused' AND u.status_code=200 ORDER BY u.created_at DESC LIMIT 1;"))
finally:c.close()
op=urllib.request.build_opener(urllib.request.ProxyHandler({}))
h={'Authorization':'Bearer '+token,'X-Client-Device-Id':'aether-wan-body-audit-20261024'}
target='1d34907b-ed95-4d15-8cc4-f946d6ad4907'
results=[]
for label,id,query in [('refused_request_repeat',target,'include_bodies=true&body_field=request_body&body_format=raw')]:
 start=time.monotonic(); total=0; chunks=[]; info={'label':label,'request_id':id,'proxy':False}
 try:
  with op.open(urllib.request.Request(base+'/api/admin/usage/'+id+'?'+query,headers=h),timeout=12) as r:
   info.update(http=r.status,ttfb_seconds=round(time.monotonic()-start,3),transfer_encoding=r.headers.get('Transfer-Encoding'),body_encoding=r.headers.get('x-aether-body-encoding'))
   print('HEADERS',json.dumps(info),flush=True)
   while True:
    chunk=r.read(65536)
    if not chunk:break
    total+=len(chunk);chunks.append(chunk)
    if time.monotonic()-start>34:
     info['deadline_exceeded']=True;break
   else: pass
   if not info.get('deadline_exceeded'):
    data=b''.join(chunks)
    if info.get('body_encoding')=='gzip':
     data=gzip.decompress(data)
     obj=json.loads(data)
     info.update(decoded_bytes=len(data),json_valid=True,top_level_keys=list(obj.keys()) if isinstance(obj,dict) else [])
 except Exception as exc:
  info.update(error=type(exc).__name__)
 info.update(total_seconds=round(time.monotonic()-start,3),received_bytes=total)
 results.append(info);print('RESULT',json.dumps(info),flush=True)
Path(__file__).with_name('wan-repeat-timings.json').write_text(json.dumps(results,ensure_ascii=False,indent=2),encoding='utf8')

