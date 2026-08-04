UPDATE agent_metadata
SET icon = '/api/assets/logos/brand/tjuae.png',
    updated_at = unixepoch('now', 'subsec') * 1000
WHERE id = '632f31d2';
