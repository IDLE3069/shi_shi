-- Empty scopes show all unarchived tasks; selected scopes form a union.
, scopes AS (SELECT value FROM json_each(?5)), picked AS (
    SELECT * FROM listed
    WHERE archived IN (SELECT 0 UNION ALL SELECT 1 WHERE '归档' IN scopes) AND merged_into = ''
        AND CASE WHEN archived THEN generation = ''
            ELSE NOT EXISTS (SELECT 1 FROM scopes) OR '全部' IN scopes OR coalesce(focus, status) IN scopes
                OR ('未完成' IN scopes AND status IN ('待处理', '处理中')) END
        AND (?6 = '' OR CASE WHEN ?6 = 'today' THEN focus IS NOT NULL AND due_ms IS NOT NULL
                AND (due_ms + 28800000) / 86400000 = (?1 + 28800000) / 86400000
            ELSE focus IS ?6 END)
)
