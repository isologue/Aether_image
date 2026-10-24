import sys,json,shlex,time,re
from pathlib import Path
sys.path.insert(0,str(Path(r"G:\Demo\Aether\output\online-audit-20261023")))
from remote import connect,sql,run
c=connect()
try:
    print('ACTIVITY',sql(c,"SELECT json_build_object('state',state,'wait_type',wait_event_type,'wait',wait_event,'elapsed_s',EXTRACT(EPOCH FROM now()-query_start)::int,'query',LEFT(query,150)) FROM pg_stat_activity WHERE datname=current_database() AND pid<>pg_backend_pid() AND state <> 'idle' ORDER BY query_start LIMIT 5;"))
    logs=run(c,"docker logs --since 45m aether-app 2>&1 | grep -E 'd3466628|1d34907b|failed to resolve admin usage' | tail -n 100")
    Path(__file__).with_name('target-server.log').write_text(logs,encoding='utf8')
    for line in logs.splitlines():
        if 'http_request_completed' in line or 'failed to resolve admin usage' in line:
            kv=dict(re.findall(r'(\w+)=("[^"\n]*"|[^\s]+)',line))
            print('HTTP',json.dumps({'time':line[:33],**{k:kv[k] for k in ['status_code','path','elapsed_ms','error','field'] if k in kv}}))
    remote_probe=r"""
import subprocess,json,time,urllib.request,urllib.error
info=json.loads(subprocess.check_output(['docker','inspect','aether-app']))[0]
e={k:v for k,v in (s.split('=',1) for s in info['Config']['Env'] if '=' in s)}
base='http://127.0.0.1:'+e.get('APP_PORT','8084')
opener=urllib.request.build_opener(urllib.request.ProxyHandler({}))
login=json.dumps({'email':e.get('ADMIN_USERNAME') or e.get('ADMIN_EMAIL') or 'admin','password':e.get('ADMIN_PASSWORD','')}).encode()
try:
    with opener.open(urllib.request.Request(base+'/api/auth/login',data=login,headers={'Content-Type':'application/json','X-Client-Device-Id':'aether-body-audit-20261024'}),timeout=15) as r:
        token=json.load(r).get('access_token')
except urllib.error.HTTPError as exc:
    print(json.dumps({'stage':'login','status':exc.code})); raise SystemExit(0)
if not token:
    print('No session token'); raise SystemExit(0)
print(json.dumps({'stage':'login','status':200,'configured_public_url':e.get('PUBLIC_URL') or e.get('APP_URL')}))
id='1d34907b-ed95-4d15-8cc4-f946d6ad4907'
for label,query in [('detail','include_bodies=false'),('request','include_bodies=true&body_field=request_body&body_format=raw'),('provider_request','include_bodies=true&body_field=provider_request_body&body_format=raw'),('response','include_bodies=true&body_field=client_response_body&body_format=raw')]:
    start=time.monotonic()
    try:
        req=urllib.request.Request(base+'/api/admin/usage/'+id+'?'+query,headers={'Authorization':'Bearer '+token,'X-Client-Device-Id':'aether-body-audit-20261024'})
        with opener.open(req,timeout=20) as r:
            ttfb=time.monotonic()-start
            data=r.read()
            print(json.dumps({'stage':label,'http':r.status,'ttfb_seconds':round(ttfb,3),'total_seconds':round(time.monotonic()-start,3),'bytes':len(data),'body_encoding':r.headers.get('x-aether-body-encoding'),'content_length':r.headers.get('content-length')}),flush=True)
    except Exception as exc:
        print(json.dumps({'stage':label,'error':type(exc).__name__,'seconds':round(time.monotonic()-start,3)}),flush=True)
"""
    stdin,out,err=c.exec_command('python3 -u -',timeout=80)
    stdin.write(remote_probe);stdin.channel.shutdown_write()
    for line in out: print('PROBE',line.strip(),flush=True)
    status=out.channel.recv_exit_status()
    if status: print('PROBE_EXIT',status,err.read().decode()[-300:])
finally:c.close()

