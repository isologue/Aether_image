import sys,json,shlex
from pathlib import Path
sys.path.insert(0,str(Path(r"G:\Demo\Aether\output\online-audit-20261023")))
from remote import connect,run
c=connect()
try:
 probe=r"""
import subprocess,json,gzip,base64,time
q="SELECT encode(payload_gzip,'base64') FROM usage_body_blobs WHERE request_id='d3466628-e070-4cfb-9502-d92a2225a3b1' AND body_field='request_body';"
args=['docker','exec','-i','aether-postgres','sh','-c','exec psql -X -A -t -v ON_ERROR_STOP=1 -U "$POSTGRES_USER" -d "$POSTGRES_DB"']
raw=subprocess.check_output(args,input=q.encode())
data=gzip.decompress(base64.b64decode(raw));obj=json.loads(data)
def shape(x,depth=0):
 if isinstance(x,dict):return {k:shape(v,depth+1) for k,v in x.items()}
 if isinstance(x,list):return {'items':len(x),'first':shape(x[0],depth+1) if x else None}
 if isinstance(x,str):return {'type':'string','characters':len(x),'data_url':x.startswith('data:')}
 return {'type':type(x).__name__}
print(json.dumps({'valid_gzip_json':True,'decoded_bytes':len(data),'shape':shape(obj)},ensure_ascii=False))
"""
 stdin,out,err=c.exec_command('python3 -',timeout=25)
 stdin.write(probe);stdin.channel.shutdown_write()
 result=out.read().decode()
 if out.channel.recv_exit_status():raise RuntimeError(err.read().decode()[-200:])
 print(result)
 Path(__file__).with_name('request-body-shape.json').write_text(result,encoding='utf8')
finally:c.close()
