ALTER TABLE system_settings
ADD COLUMN network_proxy_mode TEXT NOT NULL DEFAULT 'follow_system';

ALTER TABLE system_settings
ADD COLUMN network_proxy_url TEXT;

ALTER TABLE system_settings
ADD COLUMN network_proxy_no_proxy TEXT NOT NULL DEFAULT 'localhost,127.0.0.1,::1';
