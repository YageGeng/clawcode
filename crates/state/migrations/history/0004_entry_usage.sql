-- The usage of each assistant entry, extracted once so the trajectory sums
-- the usage before a page without re-reading every entry's whole JSON.
ALTER TABLE entries ADD COLUMN usage_json TEXT;
UPDATE entries
SET usage_json = json_extract(entry_json, '$.content.metadata.usage')
WHERE message_type = 'assistant';

-- No query reads records by Run.
DROP INDEX idx_records_by_run;
