import sys, json
from pathlib import Path
sys.path.insert(0,str(Path(r"G:\Demo\Aether\output\online-audit-20261023")))
from remote import connect, sql, run
c=connect()
try:
    print('RUNTIME', run(c, "date -Is; docker ps --filter name=aether --format '{{.Names}}\t{{.Image}}\t{{.Status}}'"))
    print('SCHEMA', sql(c, "SELECT json_build_object('table',table_name,'columns',json_agg(column_name ORDER BY ordinal_position)) FROM information_schema.columns WHERE table_schema='public' AND table_name IN ('usage','usage_http_audits','usage_body_blobs') GROUP BY table_name;"))
    print('TARGET', sql(c, "SELECT json_build_object('id',id,'request_id',request_id,'created_at',created_at,'status',status,'status_code',status_code,'is_stream',is_stream,'metadata_keys',ARRAY(SELECT jsonb_object_keys(COALESCE(request_metadata::jsonb,'{}'::jsonb))),'response_outcome',request_metadata->>'response_outcome') FROM usage WHERE request_id LIKE 'd3466628%' OR (created_at >= '2026-10-24 11:10:44+00'::timestamptz AND created_at < '2026-10-24 11:10:49+00'::timestamptz) ORDER BY created_at;"))
finally: c.close()
