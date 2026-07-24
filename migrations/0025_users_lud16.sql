-- Lightning Address (LUD-16) published via kind:0 `lud16`.
-- Nullable; absolute-state semantics match nip05_handle / about (absent → NULL).
ALTER TABLE users ADD COLUMN lud16 TEXT;
