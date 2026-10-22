CREATE TABLE IF NOT EXISTS image_tasks (
    id TEXT PRIMARY KEY,
    api_key_id TEXT NOT NULL,
    user_id TEXT NOT NULL,
    node_id TEXT NOT NULL,
    idempotency_key TEXT,
    fingerprint TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'queued' CHECK (status IN ('queued','running','succeeded','failed')),
    created_at BIGINT NOT NULL,
    started_at BIGINT,
    completed_at BIGINT,
    lease_owner TEXT,
    lease_until BIGINT,
    settled BOOLEAN NOT NULL DEFAULT FALSE,
    payload JSONB NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS image_tasks_idempotency ON image_tasks(api_key_id,idempotency_key) WHERE idempotency_key IS NOT NULL;
CREATE INDEX IF NOT EXISTS image_tasks_request ON image_tasks((payload->>'request_id'));
CREATE INDEX IF NOT EXISTS image_tasks_queue ON image_tasks(status,node_id,created_at);
CREATE INDEX IF NOT EXISTS image_tasks_retention ON image_tasks(completed_at) WHERE settled AND status IN ('succeeded','failed');

-- All configuration entrypoints (including import/reset) share the same invariant.
CREATE OR REPLACE FUNCTION aether_image_config_lock() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
  PERFORM pg_advisory_xact_lock(1095062602);
  RETURN NULL;
END $$;
DROP TRIGGER IF EXISTS image_config_lock ON system_configs;
CREATE TRIGGER image_config_lock BEFORE INSERT OR UPDATE OR DELETE ON system_configs
FOR EACH STATEMENT EXECUTE FUNCTION aether_image_config_lock();

CREATE OR REPLACE FUNCTION aether_validate_image_task_config() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE days jsonb; storage jsonb; queue jsonb; hours numeric; minimum numeric;
BEGIN
  SELECT value INTO days FROM system_configs WHERE key='async_task_retention_days';
  days := COALESCE(days,'30'::jsonb);
  IF jsonb_typeof(days) <> 'number' OR (days #>> '{}') !~ '^[0-9]+$' OR (days #>> '{}')::numeric < 1 OR (days #>> '{}')::numeric > 36500 THEN
    RAISE EXCEPTION USING ERRCODE='A0001',MESSAGE='Async task retention must be an integer between 1 and 36500 days';
  END IF;
  SELECT value INTO storage FROM system_configs WHERE key='image_storage';
  IF storage IS NOT NULL THEN
    IF jsonb_typeof(storage)<>'object'
       OR (storage ? 'cleanup_enabled' AND jsonb_typeof(storage->'cleanup_enabled')<>'boolean')
       OR (storage ? 'retention_hours' AND (jsonb_typeof(storage->'retention_hours')<>'number' OR (storage->>'retention_hours') !~ '^[0-9]+$' OR (storage->>'retention_hours')::numeric < 1 OR (storage->>'retention_hours')::numeric > 87600)) THEN
      RAISE EXCEPTION USING ERRCODE='A0001',MESSAGE='Invalid image storage retention configuration';
    END IF;
  END IF;
  IF COALESCE((storage->>'cleanup_enabled')::boolean,false) THEN
    hours := COALESCE((storage->>'retention_hours')::numeric,168);
    minimum := GREATEST(1,CEIL(hours/24));
    IF (days #>> '{}')::numeric < minimum THEN
      RAISE EXCEPTION USING ERRCODE='A0001',MESSAGE=format('Async task retention must be at least %s days for the image retention hours',minimum);
    END IF;
  END IF;
  SELECT value INTO queue FROM system_configs WHERE key='image_async_tasks';
  IF queue IS NOT NULL THEN
    IF jsonb_typeof(queue) <> 'object' THEN
      RAISE EXCEPTION USING ERRCODE='A0001',MESSAGE='Invalid image async queue configuration';
    END IF;
    IF EXISTS (SELECT 1 FROM jsonb_object_keys(queue) k WHERE k NOT IN ('concurrency','max_queued','max_queue_seconds')) THEN
      RAISE EXCEPTION USING ERRCODE='A0001',MESSAGE='Invalid image async queue configuration';
    END IF;
    IF EXISTS (SELECT 1 FROM jsonb_each(queue) kv WHERE jsonb_typeof(kv.value)<>'number') THEN
      RAISE EXCEPTION USING ERRCODE='A0001',MESSAGE='Image queue limits must be integer numbers';
    END IF;
    IF COALESCE(queue->>'concurrency','200') !~ '^[0-9]+$' OR COALESCE(queue->>'max_queued','10000') !~ '^[0-9]+$' OR COALESCE(queue->>'max_queue_seconds','3600') !~ '^[0-9]+$' THEN
      RAISE EXCEPTION USING ERRCODE='A0001',MESSAGE='Image queue limits must be positive integers';
    END IF;
    IF COALESCE((queue->>'concurrency')::numeric,200) NOT BETWEEN 1 AND 10000 OR COALESCE((queue->>'max_queued')::numeric,10000) NOT BETWEEN 1 AND 1000000 OR COALESCE((queue->>'max_queue_seconds')::numeric,3600) NOT BETWEEN 1 AND 604800 THEN
      RAISE EXCEPTION USING ERRCODE='A0001',MESSAGE='Image queue limits are outside the allowed range';
    END IF;
  END IF;
  RETURN NULL;
END $$;
DROP TRIGGER IF EXISTS image_task_config_consistency ON system_configs;
CREATE CONSTRAINT TRIGGER image_task_config_consistency AFTER INSERT OR UPDATE OR DELETE ON system_configs
DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION aether_validate_image_task_config();
