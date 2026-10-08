# Chinook

The Chinook sample database (a digital music store), version 1.4.5, from
[lerocha/chinook-database](https://github.com/lerocha/chinook-database)
(`ChinookDatabase/DataSources/Chinook_PostgreSql.sql`, SHA-256
`e3fde5c1a5b51a2a91429a702c9ca6e69ba56e6c7f5e112724d70c3d03db695e`), used unchanged by the end-to-end tests.
The tests skip its `CREATE DATABASE` and `\c` lines and run the rest in a schema of their own.

Copyright (c) 2008-2024 Luis Rocha, MIT License; see [LICENSE.md](LICENSE.md).
