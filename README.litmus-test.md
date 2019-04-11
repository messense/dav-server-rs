
# Webdav protocol compliance.

The standard for webdav compliance testing is [`litmus`](https://github.com/tolsen/litmus),
which is available at [https://github.com/tolsen/litmus](https://github.com/tolsen/litmus).

Building it:
```
git clone https://github.com/tolsen/litmus
cd litmus
./configure
make
```

Then run the test server (`sample-litmus-server`). For some tests, `litmus`
assumes that it is using basic authentication, so you must run the server
with the `--auth` flag.
```
cd webdav-handler-rs
cargo run --example sample-litmus-server -- --auth
```

You do not have to install the litmus binary, it's possible to run the tests
straight from the unpacked & compiled litmus directory (`someuser` and
`somepass` are literal, you do not have to put a real username/password there):

```
$ cd litmus
$ TESTS="http basic copymove locks props" HTDOCS=htdocs TESTROOT=. \
	./litmus http://localhost:4918/ someuser somepass

-> running `basic':
 0. init.................. pass
 1. begin................. pass
 2. options............... pass
 3. put_get............... pass
 4. put_get_utf8_segment.. pass
 5. mkcol_over_plain...... pass
 6. delete................ pass
 7. delete_null........... pass
 8. delete_fragment....... WARNING: DELETE removed collection resource with Request-URI including fragment; unsafe
    ...................... pass (with 1 warning)
 9. mkcol................. pass
10. mkcol_percent_encoded. pass
11. mkcol_again........... pass
12. delete_coll........... pass
13. mkcol_no_parent....... pass
14. mkcol_with_body....... pass
15. mkcol_forbidden....... pass
16. chk_ETag.............. pass
17. finish................ pass
<- summary for `basic': of 18 tests run: 18 passed, 0 failed. 100.0%
-> 1 warning was issued.
-> running `copymove':
 0. init.................. pass
 1. begin................. pass
 2. copy_init............. pass
 3. copy_simple........... pass
 4. copy_overwrite........ pass
 5. copy_nodestcoll....... pass
 6. copy_cleanup.......... pass
 7. copy_content_check.... pass
 8. copy_coll_depth....... pass
 9. copy_coll............. pass
10. depth_zero_copy....... pass
11. copy_med_on_coll...... pass
12. move.................. pass
13. move_coll............. pass
14. move_cleanup.......... pass
15. move_content_check.... pass
16. move_collection_check. pass
17. finish................ pass
<- summary for `copymove': of 18 tests run: 18 passed, 0 failed. 100.0%
-> running `props':
 0. init.................. pass
 1. begin................. pass
 2. propfind_invalid...... pass
 3. propfind_invalid2..... pass
 4. propfind_d0........... pass
 5. propinit.............. pass
 6. propfind_d1........... pass
 7. proppatch_invalid_semantics...................... pass
 8. propset............... pass
 9. propget............... pass
10. propfind_empty........ WARNING: Server did not return the property: displayname
WARNING: Server did not return the property: getcontentlanguage
    ...................... pass (with 2 warnings)
11. propfind_allprop_include...................... WARNING: Server did not return the property: displayname
WARNING: Server did not return the property: getcontentlanguage
WARNING: Server did not return the property: acl
WARNING: Server did not return the property: resource-id
    ...................... pass (with 4 warnings)
12. propfind_propname..... WARNING: Server did not return the property: displayname
WARNING: Server did not return the property: getcontentlanguage
WARNING: Server did not return the property: acl
WARNING: Server did not return the property: resource-id
    ...................... pass (with 4 warnings)
13. proppatch_liveunprotect...................... pass
14. propextended.......... pass
15. propcopy.............. pass
16. propget............... pass
17. propcopy_unmapped..... pass
18. propget............... pass
19. propmove.............. pass
20. propget............... pass
21. propdeletes........... pass
22. propget............... pass
23. propreplace........... pass
24. propget............... pass
25. propnullns............ pass
26. propget............... pass
27. prophighunicode....... pass
28. propget............... pass
29. propvalnspace......... pass
30. propwformed........... pass
31. propinit.............. pass
32. propmanyns............ pass
33. propget............... pass
34. property_mixed........ pass
35. propfind_mixed........ pass
36. propcleanup........... pass
37. finish................ pass
<- summary for `props': of 38 tests run: 38 passed, 0 failed. 100.0%
-> 10 warnings were issued.
-> running `locks':
 0. init.................. pass
 1. begin................. pass
 2. options............... pass
 3. precond............... pass
 4. init_locks............ pass
 5. lock_on_no_file....... pass
 6. double_sharedlock..... pass
 7. supportedlock......... pass
 8. unlock_on_no_file..... pass
 9. put................... pass
10. lock_excl............. pass
11. lock_excl_fail........ pass
12. lockdiscovery......... pass
13. discover.............. pass
14. refresh............... pass
15. notowner_modify....... pass
16. notowner_lock......... pass
17. owner_modify.......... pass
18. notowner_modify....... pass
19. notowner_lock......... pass
20. copy.................. pass
21. cond_put.............. pass
22. fail_cond_put......... pass
23. cond_put_with_not..... pass
24. cond_put_corrupt_token pass
25. complex_cond_put...... pass
26. fail_complex_cond_put. pass
27. unlock................ pass
28. fail_cond_put_unlocked pass
29. lock_shared........... pass
30. lock_excl_fail........ pass
31. notowner_modify....... pass
32. notowner_lock......... pass
33. owner_modify.......... pass
34. double_sharedlock..... pass
35. lock_excl_fail........ pass
36. notowner_modify....... pass
37. notowner_lock......... pass
38. cond_put.............. pass
39. fail_cond_put......... pass
40. cond_put_with_not..... pass
41. cond_put_corrupt_token pass
42. complex_cond_put...... pass
43. fail_complex_cond_put. pass
44. unlock................ pass
45. lock_infinite......... pass
46. lockdiscovery......... pass
47. supportedlock......... pass
48. notowner_modify....... pass
49. notowner_lock......... pass
50. discover.............. pass
51. refresh............... pass
52. unlock_fail........... pass
53. lock_invalid_depth.... pass
54. unlock................ pass
55. prep_collection....... pass
56. conflicting_locks..... pass
57. lock_collection....... pass
58. supportedlock......... pass
59. owner_modify.......... pass
60. notowner_modify....... pass
61. newowner_modify_notoken...................... pass
62. newowner_modify_correcttoken...................... pass
63. refresh............... pass
64. indirect_refresh...... pass
65. unlock................ pass
66. unmap_lockroot........ pass
67. lockcleanup........... pass
68. finish................ pass
<- summary for `locks': of 69 tests run: 69 passed, 0 failed. 100.0%
-> running `http':
 0. init.................. pass
 1. begin................. pass
 2. expect100............. pass
 3. finish................ pass
<- summary for `http': of 4 tests run: 4 passed, 0 failed. 100.0%

```

## Copyright and License.

 * © 2018, 2019 XS4ALL Internet bv
 * © 2018, 2019 Miquel van Smoorenburg
 * [Apache License, Version 2.0](http://www.apache.org/licenses/LICENSE-2.0)

