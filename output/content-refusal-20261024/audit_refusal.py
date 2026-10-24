import sys,json
from pathlib import Path
sys.path.insert(0,str(Path(r"G:\Demo\Aether\output\online-audit-20261023")))
from remote import connect,sql
q="""SELECT json_build_object('request_id',request_id,'created_at',created_at,'status',status,'http_status',status_code,'model',model,'api_format',api_format,'is_stream',is_stream,'total_cost_usd',total_cost_usd,'billing_status',billing_status,'metadata',request_metadata) FROM usage WHERE created_at >= '2026-10-24 09:15:20+00'::timestamptz AND created_at < '2026-10-24 09:15:30+00'::timestamptz ORDER BY created_at;"""
c=connect()
try:
 raw=sql(c,q)
 Path(__file__).with_name('target-usage.jsonl').write_text(raw,encoding='utf-8')
 for line in raw.splitlines():
  obj=json.loads(line)
  m=obj.pop('metadata',{}) or {}
  obj['metadata_keys']=list(m.keys()) if isinstance(m,dict) else []
  if isinstance(m,dict):
   obj['refusal_metadata']={k:v for k,v in m.items() if any(x in k.lower() for x in ['refus','outcome','async','path'])}
  print(json.dumps(obj,ensure_ascii=False))
finally:c.close()
