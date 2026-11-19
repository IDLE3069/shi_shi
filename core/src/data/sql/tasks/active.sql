-- Deferred tasks keep their priority slots, filled in return order.
, ordered AS (
    SELECT *, row_number() OVER (ORDER BY level_rank, updated DESC, number) AS slot,
        row_number() OVER (PARTITION BY focus IS '延后处理' ORDER BY level_rank, updated DESC, number) AS nth_slot,
        row_number() OVER (PARTITION BY focus IS '延后处理'
            ORDER BY deferred_until, level_rank, updated DESC, number) AS nth_return
    FROM picked
)
SELECT o.*, coalesce(s.slot, o.slot) AS k0 FROM ordered o
LEFT JOIN ordered s ON o.focus IS '延后处理' AND s.focus IS '延后处理' AND s.nth_slot = o.nth_return
