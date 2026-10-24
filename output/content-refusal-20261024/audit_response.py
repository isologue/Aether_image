import sys,json
from pathlib import Path
sys.path.insert(0,str(Path(r"G:\Demo\Aether\output\online-audit-20261023")))
from remote import connect,sql,run
c=connect()
try:
 print(run(c,"docker ps --format '{{.Names}}\t{{.Image}}'"))
 print(sql(c,"SELECT json_build_object('audit',to_jsonb(a)) FROM usage_http_audits a WHERE request_id='00791b7e-9e93-4aa0-b3e4-2936aacb3ba7'; SELECT json_build_object('table', table_name, 'columns',json_agg(column_name ORDER BY ordinal_position)) FROM information_schema.columns WHERE table_schema='public' AND table_name LIKE '%body%' GROUP BY table_name;"))
finally:c.close()
