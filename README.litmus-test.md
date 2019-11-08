
# Webdav protocol compliance.

The standard for webdav compliance testing is [`litmus`](http://www.webdav.org/neon/litmus/),
which is available at [http://www.webdav.org/neon/litmus/](http://www.webdav.org/neon/litmus/).

Building it:
```
curl -O http://www.webdav.org/neon/litmus/litmus-0.13.tar.gz
tar xf litmus-0.13.tar.gz
cd litmus-0.13
./configure
make
```

Then run the test server (`sample-litmus-server`). For some tests, `litmus`
assumes that it is using basic authentication, so you must run the server
with the `--auth` flag.
```
cd webdav-handler-rs
cargo run --example sample-litmus-server -- --memfs --auth
```

You do not have to install the litmus binary, it's possible to run the tests
straight from the unpacked & compiled litmus directory (`someuser` and
`somepass` are literal, you do not have to put a real username/password there):

```
$ cd litmus-0.13
$ TESTS="http basic copymove locks props" HTDOCS=htdocs TESTROOT=. ./litmus http://localhost:4918/ someuser somepass

-> running `http':
 0. init.................. pass
 1. begin................. pass
 2. expect100............. pass
 3. finish................ pass
<- summary for `http': of 4 tests run: 4 passed, 0 failed. 100.0%
-> running `basic':
 0. init.................. pass
 1. begin................. pass
 2. options............... pass
 3. put_get............... pass
 4. put_get_utf8_segment.. pass
 5. put_no_parent......... pass
 6. mkcol_over_plain...... pass
 7. delete................ pass
 8. delete_null........... pass
 9. delete_fragment....... WARNING: DELETE removed collection resource with Request-URI including fragment; unsafe
    ...................... pass (with 1 warning)
10. mkcol................. pass
11. mkcol_again........... pass
12. delete_coll........... pass
13. mkcol_no_parent....... pass
14. mkcol_with_body....... pass
15. finish................ pass
<- summary for `basic': of 16 tests run: 16 passed, 0 failed. 100.0%
-> 1 warning was issued.
-> running `copymove':
 0. init.................. pass
 1. begin................. pass
 2. copy_init............. pass
 3. copy_simple........... pass
 4. copy_overwrite........ pass
 5. copy_nodestcoll....... pass
 6. copy_cleanup.......... pass
 7. copy_coll............. pass
 8. copy_shallow.......... pass
 9. move.................. pass
10. move_coll............. pass
11. move_cleanup.......... pass
12. finish................ pass
<- summary for `copymove': of 13 tests run: 13 passed, 0 failed. 100.0%
-> running `locks':
 0. init.................. pass
 1. begin................. pass
 2. options............... pass
 3. precond............... pass
 4. init_locks............ pass
 5. put................... pass
 6. lock_excl............. pass
 7. discover.............. pass
 8. refresh............... pass
 9. notowner_modify....... pass
10. notowner_lock......... pass
11. owner_modify.......... pass
12. notowner_modify....... pass
13. notowner_lock......... pass
14. copy.................. pass
15. cond_put.............. pass
16. fail_cond_put......... pass
17. cond_put_with_not..... pass
18. cond_put_corrupt_token pass
19. complex_cond_put...... pass
20. fail_complex_cond_put. pass
21. unlock................ pass
22. fail_cond_put_unlocked pass
23. lock_shared........... pass
24. notowner_modify....... pass
25. notowner_lock......... pass
26. owner_modify.......... pass
27. double_sharedlock..... pass
28. notowner_modify....... pass
29. notowner_lock......... pass
30. unlock................ pass
31. prep_collection....... pass
32. lock_collection....... pass
33. owner_modify.......... pass
34. notowner_modify....... pass
35. refresh............... pass
36. indirect_refresh...... pass
37. unlock................ pass
38. unmapped_lock......... pass
39. unlock................ pass
40. finish................ pass
<- summary for `locks': of 41 tests run: 41 passed, 0 failed. 100.0%
-> running `props':
 0. init.................. pass
 1. begin................. pass
 2. propfind_invalid...... pass
 3. propfind_invalid2..... pass
 4. propfind_d0........... pass
 5. propinit.............. pass
 6. propset............... pass
 7. propget............... pass
 8. propextended.......... pass
 9. propmove.............. pass
10. propget............... pass
11. propdeletes........... pass
12. propget............... pass
13. propreplace........... pass
14. propget............... pass
15. propnullns............ pass
16. propget............... pass
17. prophighunicode....... pass
18. propget............... pass
19. propremoveset......... pass
20. propget............... pass
21. propsetremove......... pass
22. propget............... pass
23. propvalnspace......... pass
24. propwformed........... pass
25. propinit.............. pass
26. propmanyns............ pass
27. propget............... pass
28. propcleanup........... pass
29. finish................ pass
<- summary for `props': of 30 tests run: 30 passed, 0 failed. 100.0%
```

