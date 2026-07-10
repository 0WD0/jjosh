mod in_parallel_with_slice {
    use std::sync::atomic::{AtomicBool, Ordering};

    use gix_features::parallel;

    #[test]
    fn the_error_that_caused_the_stop_is_returned_even_if_other_threads_fail_on_interrupt() {
        // Regression test for the race behind issue #2701: a thread that observes the stop
        // signal and bails out with a reactive error (like `Interrupted`) must not mask the
        // error of the thread that caused the stop, no matter the join order.
        let thread0_has_item = &AtomicBool::new(false);
        let mut input = [(); 2];
        let err = parallel::in_parallel_with_slice(
            &mut input,
            Some(2),
            |thread_id| thread_id,
            |_item, thread_id, _threads_left, should_interrupt| {
                if *thread_id == 0 {
                    // Ensure the causal error strikes only once this thread holds an item,
                    // so it always produces a reactive error afterwards.
                    thread0_has_item.store(true, Ordering::Relaxed);
                    while !should_interrupt.load(Ordering::Relaxed) {
                        std::thread::yield_now();
                    }
                    Err("reaction to the stop signal")
                } else {
                    while !thread0_has_item.load(Ordering::Relaxed) {
                        std::thread::yield_now();
                    }
                    Err("the cause of the stop")
                }
            },
            || Some(std::time::Duration::from_millis(10)),
            |thread_id| thread_id,
        )
        .expect_err("failing consumers lead to an error");
        assert_eq!(
            err, "the cause of the stop",
            "the causal error wins over errors of threads that merely reacted to the stop, \
             even though they are joined first"
        );
    }

    #[test]
    fn the_error_of_a_consumer_that_set_the_stop_flag_itself_is_still_the_cause() {
        // Consumers receive the stop flag and may set it themselves right before failing,
        // e.g. to stop nested or peer work immediately. Their error is still the cause of
        // the stop and must win over reactive errors of threads that are joined earlier.
        let thread0_has_item = &AtomicBool::new(false);
        let mut input = [(); 2];
        let err = parallel::in_parallel_with_slice(
            &mut input,
            Some(2),
            |thread_id| thread_id,
            |_item, thread_id, threads_left, should_interrupt| {
                if *thread_id == 0 {
                    thread0_has_item.store(true, Ordering::Relaxed);
                    while !should_interrupt.load(Ordering::Relaxed) {
                        std::thread::yield_now();
                    }
                    // Wait for the causing thread to have failed for good, observable by it
                    // handing back its slot in `threads_left`, to assure this reactive error
                    // strictly follows the causal one.
                    while threads_left.load(Ordering::SeqCst) < 1 {
                        std::thread::yield_now();
                    }
                    Err("reaction to the stop signal")
                } else {
                    while !thread0_has_item.load(Ordering::Relaxed) {
                        std::thread::yield_now();
                    }
                    should_interrupt.store(true, Ordering::Relaxed);
                    Err("the cause of the stop")
                }
            },
            || Some(std::time::Duration::from_millis(10)),
            |thread_id| thread_id,
        )
        .expect_err("failing consumers lead to an error");
        assert_eq!(
            err, "the cause of the stop",
            "causality is not determined by the stop flag, which may be set by consumers themselves"
        );
    }
}

mod optimize_chunk_size_and_thread_limit {
    use gix_features::parallel::optimize_chunk_size_and_thread_limit;

    #[test]
    fn not_enough_chunks_for_threads() {
        assert_eq!(
            optimize_chunk_size_and_thread_limit(1, Some(10), None, Some(10)),
            (1, Some(5), 5)
        );
        assert_eq!(
            optimize_chunk_size_and_thread_limit(1, Some(10), Some(3), Some(10)),
            (1, Some(3), 3),
            "the thread limit is always respected"
        );
    }

    #[test]
    fn some_more_chunks_per_thread() {
        assert_eq!(
            optimize_chunk_size_and_thread_limit(1, Some(30), None, Some(10)),
            (1, Some(10), 10)
        );
        assert_eq!(
            optimize_chunk_size_and_thread_limit(1, Some(30), Some(5), Some(10)),
            (3, Some(5), 5),
            "the thread limit is always respected"
        );
    }

    #[test]
    fn chunk_size_too_small() {
        assert_eq!(
            optimize_chunk_size_and_thread_limit(1, Some(100), None, Some(10)),
            (5, Some(10), 10)
        );
        assert_eq!(
            optimize_chunk_size_and_thread_limit(1, Some(100), Some(5), Some(10)),
            (10, Some(5), 5),
            "the thread limit is always respected"
        );
    }

    #[test]
    fn chunk_size_too_big() {
        assert_eq!(
            optimize_chunk_size_and_thread_limit(50, Some(100), None, Some(10)),
            (5, Some(10), 10)
        );
        assert_eq!(
            optimize_chunk_size_and_thread_limit(50, Some(100), Some(5), Some(10)),
            (10, Some(5), 5),
            "the thread limit is always respected"
        );
    }

    mod unknown_chunk_count {
        use gix_features::parallel::optimize_chunk_size_and_thread_limit;

        #[test]
        fn medium_chunk_size_many_threads() {
            assert_eq!(
                optimize_chunk_size_and_thread_limit(50, None, None, Some(4)),
                (50, Some(4), 4),
                "really, what do we know"
            );
        }

        #[test]
        fn medium_chunk_size_single_thread() {
            assert_eq!(
                optimize_chunk_size_and_thread_limit(50, None, None, Some(1)),
                (50, Some(1), 1),
                "single threaded - we don't touch that"
            );
        }

        #[test]
        fn small_chunk_size_single_thread() {
            assert_eq!(
                optimize_chunk_size_and_thread_limit(1, None, None, Some(1)),
                (1, Some(1), 1),
                "single threaded - we don't touch that"
            );
        }

        #[test]
        fn small_chunk_size_many_threads() {
            assert_eq!(
                optimize_chunk_size_and_thread_limit(1, None, None, Some(4)),
                (50, Some(4), 4),
                "we prefer an arbitrary number, which should really be based on effort, but the caller has to adjust for that"
            );
        }
    }

    mod real_values {
        use gix_features::parallel::optimize_chunk_size_and_thread_limit;

        #[test]
        fn linux_kernel_pack_my_machine_lookup() {
            assert_eq!(
                optimize_chunk_size_and_thread_limit(10000, Some(7_500_000), None, Some(4)),
                (1000, Some(4), 4),
                "the bucket size is capped actually, somewhat arbitrarily"
            );
        }

        #[test]
        fn linux_kernel_pack_my_machine_indexed() {
            assert_eq!(
                optimize_chunk_size_and_thread_limit(1, None, None, Some(4)),
                (50, Some(4), 4),
                "low values are raised to arbitrary value"
            );
            assert_eq!(
                optimize_chunk_size_and_thread_limit(10000, None, None, Some(4)),
                (1000, Some(4), 4),
                "high values are capped"
            );
        }
    }
}
