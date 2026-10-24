import sys,json,re
from pathlib import Path
sys.path.insert(0,str(Path(r"G:\Demo\Aether\output\online-audit-20261023")))
from remote import connect,sql,run
c=connect()
try:
 print('PORTS',run(c,"docker ps --format '{{.Names}}\t{{.Ports}}'"))
 print('ROUTE_HEADERS',sql(c,"SELECT json_build_object('host',COALESCE(a.request_headers,u.request_headers)->>'host','forwarded_host',COALESCE(a.request_headers,u.request_headers)->>'x-forwarded-host','origin',COALESCE(a.request_headers,u.request_headers)->>'origin') FROM usage u LEFT JOIN usage_http_audits a USING(request_id) WHERE u.request_id='d3466628-e070-4cfb-9502-d92a2225a3b1';"))
 print('BODY_SHAPE',sql(c,"SELECT json_build_object('keys',jsonb_object_keys(request_metadata::jsonb)) FROM usage WHERE request_id='d3466628-e070-4cfb-9502-d92a2225a3b1' AND request_metadata::jsonb ? 'body_size';"))
 print('PROXY_CONFIG',run(c,"if command -v nginx >/dev/null 2>&1; then nginx -T 2>/dev/null | awk '/server_name|listen |proxy_pass|limit_rate|proxy_buffering|proxy_read_timeout|send_timeout|gzip / {print}'; fi"))
 print('IMAGE',run(c,"docker inspect aether-app --format '{{.Image}} {{json .NetworkSettings.Ports}}'"))
finally:c.close()
