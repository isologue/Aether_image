import sys,json,gzip,base64
from pathlib import Path
sys.path.insert(0,str(Path(r"G:\Demo\Aether\output\online-audit-20261023")))
from remote import connect,sql
c=connect()
try:
 raw=sql(c,"SELECT json_build_object('field',body_field,'payload',encode(payload_gzip,'base64')) FROM usage_body_blobs WHERE request_id='00791b7e-9e93-4aa0-b3e4-2936aacb3ba7' AND body_field IN ('response_body','client_response_body');")
 for line in raw.splitlines():
  row=json.loads(line); data=gzip.decompress(base64.b64decode(row['payload'])).decode('utf-8','replace')
  Path(__file__).with_name(row['field']+'.txt').write_text(data,encoding='utf-8')
  print(row['field'],len(data),data[:2400])
finally:c.close()
