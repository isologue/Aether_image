-- Old records are backfilled in bounded batches on first OAuth admission.
ALTER TABLE provider_api_keys ADD COLUMN oauth_identity_indexed BOOLEAN NOT NULL DEFAULT FALSE;
CREATE INDEX idx_provider_oauth_identity_pending ON provider_api_keys(provider_id, id)
    WHERE NOT oauth_identity_indexed;
CREATE TABLE provider_oauth_identity_index (
    key_id VARCHAR(36) PRIMARY KEY REFERENCES provider_api_keys(id) ON DELETE CASCADE,
    provider_id VARCHAR(36) NOT NULL REFERENCES providers(id) ON DELETE CASCADE,
    identity_hashes TEXT[] NOT NULL
);
CREATE INDEX idx_provider_oauth_identity_provider ON provider_oauth_identity_index(provider_id, key_id);
CREATE INDEX idx_provider_oauth_identity_hashes ON provider_oauth_identity_index USING GIN(identity_hashes);

-- Credential CAS and older writers invalidate projections too. New writers rebuild
-- the projection before committing the same transaction as the credential change.
CREATE FUNCTION invalidate_provider_oauth_identity_index() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    DELETE FROM provider_oauth_identity_index WHERE key_id = OLD.id;
    NEW.oauth_identity_indexed := FALSE;
    RETURN NEW;
END;
$$;
CREATE TRIGGER provider_oauth_identity_invalidation
BEFORE UPDATE OF auth_config, provider_id, auth_type ON provider_api_keys
FOR EACH ROW WHEN (OLD.auth_config IS DISTINCT FROM NEW.auth_config
    OR OLD.provider_id IS DISTINCT FROM NEW.provider_id OR OLD.auth_type IS DISTINCT FROM NEW.auth_type)
EXECUTE FUNCTION invalidate_provider_oauth_identity_index();
