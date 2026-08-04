UPDATE agent_metadata
SET icon = '/api/assets/logos/brand/tjuae.svg',
    updated_at = unixepoch('now', 'subsec') * 1000
WHERE id = '632f31d2';
