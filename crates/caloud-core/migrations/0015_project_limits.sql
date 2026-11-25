-- ADR 0025：專案資源上限（NULL＝每個服務 1 GB、1 核）
ALTER TABLE projects ADD COLUMN memory_limit_mb INTEGER;
ALTER TABLE projects ADD COLUMN cpu_limit REAL;
