import sys,json,shlex,time
from pathlib import Path
sys.path.insert(0,str(Path(r"G:\Demo\Aether\output\online-audit-20261023")))
from remote import connect,sql,run
c=connect()
try:
 q="""SELECT json_build_object('id',u.id,'request_id',u.request_id,'created_at',u.created_at,'outcome',u.request_metadata->>'response_outcome','body_size',u.request_metadata->>'body_size','request_path',u.request_metadata->>'request_path','inline_request_bytes',octet_length(u.request_body::text),'request_compressed_bytes',octet_length(u.request_body_compressed),'provider_inline_request_bytes',octet_length(u.provider_request_body::text),'provider_request_compressed_bytes',octet_length(u.provider_request_body_compressed),'audit',to_jsonb(a)-'request_headers'-'provider_request_headers'-'response_headers'-'client_response_headers','blobs',(SELECT json_agg(json_build_object('field',b.body_field,'bytes',octet_length(b.payload_gzip),'ref',b.body_ref)) FROM usage_body_blobs b WHERE b.request_id=u.request_id)) FROM usage u LEFT JOIN usage_http_audits a USING(request_id) WHERE u.request_id='d3466628-e070-4cfb-9502-d92a2225a3b1' OR u.id IN (SELECT id FROM usage WHERE created_at >= '2026-10-24 11:09:00+00'::timestamptz AND created_at < '2026-10-24 11:13:00+00'::timestamptz ORDER BY created_at DESC LIMIT 8) ORDER BY u.created_at;"""
 t=time.monotonic(); result=sql(c,q)
 Path(__file__).with_name('body-states.jsonl').write_text(result,encoding='utf8')
 print('STORAGE_SECONDS',round(time.monotonic()-t,2)); print(result)
 print('ACTIVITY',sql(c,"SELECT json_build_object('state',state,'wait_type',wait_event_type,'wait',wait_event,'elapsed_s',EXTRACT(EPOCH FROM now()-query_start)::int,'query',LEFT(query,240)) FROM pg_stat_activity WHERE datname=current_database() AND pid<>pg_backend_pid() AND state <> 'idle' ORDER BY query_start LIMIT 8;"))
 print('LOGS',run(c,"docker logs --since 35m aether-app 2>&1 | grep -E 'd3466628|1d34907b|failed to resolve admin usage|usage_body|body payload' | tail -n 55"))
finally:c.close()
