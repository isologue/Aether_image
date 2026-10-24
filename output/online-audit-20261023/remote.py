from pathlib import Path
import sys, re, shlex, json
sys.path.insert(0, str(Path(__file__).resolve().parent / "deps"))
import paramiko

def connect():
    values = {}
    port = 22
    for line in Path(r"C:\Users\Milo_\Desktop\psd5.txt").read_text(encoding="utf-8-sig").splitlines():
        line = line.strip()
        if not line:
            continue
        if line.isdigit():
            port = int(line)
            continue
        match = re.match(r"(ip|host|user|username|psd|password|port)\s*(?:[:=：]\s*|\s+)(.+)", line, re.IGNORECASE)
        if match:
            values[match[1].lower()] = match[2].strip()
    host = values.get("ip") or values.get("host")
    username = values.get("user") or values.get("username") or "root"
    password = values.get("psd") or values.get("password")
    port = int(values.get("port", port))
    if not host or not password:
        raise RuntimeError("Credential format not recognized; values not displayed")
    client = paramiko.SSHClient()
    client.set_missing_host_key_policy(paramiko.AutoAddPolicy())
    client.connect(host, port=port, username=username, password=password, timeout=20,
                   auth_timeout=20, banner_timeout=20, look_for_keys=False, allow_agent=False)
    return client

def run(client, command, timeout=90):
    _, out, err = client.exec_command(command, timeout=timeout)
    data = out.read().decode("utf-8", "replace")
    errors = err.read().decode("utf-8", "replace")
    status = out.channel.recv_exit_status()
    if status:
        raise RuntimeError(f"Remote command exit {status}: {errors[:500]}")
    return data

def sql(client, query):
    inner = 'exec psql -X -q -A -t -v ON_ERROR_STOP=1 -U "$POSTGRES_USER" -d "$POSTGRES_DB"'
    command = "docker exec -i aether-postgres sh -c " + shlex.quote(inner)
    stdin, out, err = client.exec_command(command, timeout=90)
    stdin.write("BEGIN READ ONLY; SET LOCAL statement_timeout='25s';\n" + query + "\nROLLBACK;\n")
    stdin.channel.shutdown_write()
    data = out.read().decode("utf-8", "replace")
    errors = err.read().decode("utf-8", "replace")
    status = out.channel.recv_exit_status()
    if status:
        raise RuntimeError(f"Read-only query exit {status}: {errors[:700]}")
    return data

if __name__ == "__main__":
    client = connect()
    try:
        print(run(client, "date -Is; docker ps --filter name=aether --format '{{.Names}}\t{{.Image}}\t{{.Status}}'; docker inspect aether-app --format '{{.Config.Image}} {{.Image}} {{.State.StartedAt}} {{index .Config.Labels \"com.docker.compose.project.working_dir\"}}'"))
        print(sql(client, "SELECT json_build_object('table', table_name, 'columns', json_agg(column_name ORDER BY ordinal_position)) FROM information_schema.columns WHERE table_schema='public' AND table_name IN ('usage','usage_rows','request_candidates','provider_api_keys','image_tasks','usage_http_audits','usage_routing_snapshots') GROUP BY table_name ORDER BY table_name;"))
    finally:
        client.close()
