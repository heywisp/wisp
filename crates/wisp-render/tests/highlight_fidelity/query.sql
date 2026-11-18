SELECT u.id, COUNT(*) AS n
FROM users u LEFT JOIN orders o ON o.user_id = u.id
WHERE u.created_at > '2024-01-01' AND o.total >= 10.5 -- recent
GROUP BY u.id HAVING COUNT(*) > 3;
