import sys,json,shlex
from pathlib import Path
sys.path.insert(0,str(Path(r"G:\Demo\Aether\output\online-audit-20261023")))
from remote import connect,run
q="SELECT id,created_at,type,model_name,quota,request_id,JSON_EXTRACT(other,'$.request_path') AS path,JSON_EXTRACT(other,'$.async') AS is_async,JSON_EXTRACT(other,'$.response_outcome') AS outcome FROM `new-api`.`logs` WHERE created_at BETWEEN 1792833284 AND 1792833424 AND model_name='gpt-image-2' ORDER BY created_at;"
cmd='docker exec mysql_image sh -c '+shlex.quote('MYSQL_PWD="$MYSQL_ROOT_PASSWORD" exec mysql -uroot --batch -e "$1"')+' sh '+shlex.quote(q)
c=connect()
try:
 data=run(c,cmd)
 Path(__file__).with_name('moai-target-records.tsv').write_text(data,encoding='utf-8')
 print(data)
finally:c.close()
