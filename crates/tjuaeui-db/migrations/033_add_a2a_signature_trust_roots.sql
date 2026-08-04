ALTER TABLE a2a_agent_profiles
ADD COLUMN signature_trust_roots_json TEXT NOT NULL DEFAULT '[]';
