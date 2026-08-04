-- A2A 1.0 security requirements can require several credentials at once.
-- Keep the original single reference long enough to backfill existing profiles.
ALTER TABLE a2a_agent_profiles
ADD COLUMN credential_refs_json TEXT NOT NULL DEFAULT '[]';

UPDATE a2a_agent_profiles
SET credential_refs_json = json_array(credential_ref)
WHERE credential_ref IS NOT NULL
  AND credential_ref <> '';

ALTER TABLE a2a_agent_profiles
ADD COLUMN selected_tenant TEXT;

ALTER TABLE a2a_credentials
ADD COLUMN scheme_name TEXT;

-- Manual replacement of Agent Card signature trust roots was a product-owned
-- trust store rather than an interoperable A2A facility. Remove its storage.
ALTER TABLE a2a_agent_profiles
DROP COLUMN signature_trust_roots_json;
