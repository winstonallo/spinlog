-- Adds an optional bio field to user profiles.
-- SQLite supports ADD COLUMN directly for nullable columns with no default constraint issues.
ALTER TABLE users ADD COLUMN bio TEXT;
