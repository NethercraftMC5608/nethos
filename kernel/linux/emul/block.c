// SPDX-License-Identifier: GPL-2.0
/*
 * Enough of blk-mq for a driver to be given a request and complete it.
 *
 * This is the file where the shim stops being a translation of Linux and
 * starts being a *facade*: there is no block layer here at all. No bios, no
 * queues, no tags, no schedulers, no plugging, no merging. What there is, is
 * the shape blk-mq presents to a driver -- because that shape is all
 * virtio_blk.c ever sees, and it is small:
 *
 *   - the driver registers a `blk_mq_ops` when it allocates a tag set;
 *   - it is handed a `struct request` through ops->queue_rq;
 *   - it calls blk_mq_complete_request when the device answers, which is a
 *     request to call ops->complete;
 *   - ops->complete ends with blk_mq_end_request, which is where it stops
 *     being the driver's.
 *
 * nk therefore fabricates the request rather than receiving one from above.
 * `nk_blk_read` builds one, hands it to the driver's own queue_rq, and waits.
 * Everything in between -- the virtio header, the descriptor chain, the
 * notification, the interrupt, the used ring -- is the real driver's code
 * doing the real thing.
 *
 * The consequences are worth stating plainly rather than discovering:
 *
 *   - **One request in flight.** There is a single slot, so there is no tag
 *     allocation and no need for one. A second concurrent reader would
 *     silently share it.
 *   - **One segment.** The buffer is contiguous and virtually mapped, so a
 *     request is one scatterlist entry. blk_rq_map_sg reflects that, and a
 *     multi-segment request cannot currently be built to be mapped wrongly.
 *   - **No merging, no ordering, no barriers.** A single synchronous request
 *     makes all three vacuous.
 *
 * A real block layer is what would lift each of those, and it is a much
 * larger project than this file. What this file establishes is that it is the
 * only thing left between nk and a working driver.
 */

#include <linux/blk-mq.h>
#include <linux/blkdev.h>
#include <linux/scatterlist.h>
#include <linux/slab.h>
#include <linux/string.h>

#include "nk.h"

static const struct blk_mq_ops *ops;
static unsigned int cmd_size;
static struct gendisk *the_disk;
static struct blk_mq_hw_ctx hctx;

/* The single in-flight slot. See the note above. */
static struct request *inflight;
static void *inflight_buf;
static unsigned int inflight_len;
static volatile int inflight_done;
static volatile blk_status_t inflight_status;

int blk_mq_alloc_tag_set(struct blk_mq_tag_set *set)
{
	ops = set->ops;
	cmd_size = set->cmd_size;
	return 0;
}

void blk_mq_free_tag_set(struct blk_mq_tag_set *set)
{
	(void)set;
	ops = NULL;
}

struct gendisk *__blk_mq_alloc_disk(struct blk_mq_tag_set *set,
				    struct queue_limits *lim, void *queuedata,
				    struct lock_class_key *lkclass)
{
	struct request_queue *q;
	struct gendisk *disk;

	(void)set;
	(void)lkclass;
	q = nk_alloc(sizeof(*q), 8);
	disk = nk_alloc(sizeof(*disk), 8);
	if (!q || !disk)
		return ERR_PTR(-ENOMEM);
	memset(q, 0, sizeof(*q));
	memset(disk, 0, sizeof(*disk));

	if (lim)
		q->limits = *lim;
	/* virtio_queue_rq's first line: hctx->queue->queuedata is the driver's
	 * own state. Everything downstream of it depends on this one field. */
	q->queuedata = queuedata;
	disk->queue = q;
	hctx.queue = q;
	hctx.queue_num = 0;
	the_disk = disk;
	return disk;
}

int device_add_disk(struct device *parent, struct gendisk *disk,
		    const struct attribute_group **groups)
{
	(void)parent;
	(void)groups;
	the_disk = disk;
	return 0;
}

void del_gendisk(struct gendisk *disk)
{
	(void)disk;
}

void put_disk(struct gendisk *disk)
{
	(void)disk;
}

void set_disk_ro(struct gendisk *disk, bool ro)
{
	(void)disk;
	(void)ro;
}

int __register_blkdev(unsigned int major, const char *name,
		      void (*probe)(dev_t))
{
	/* Major numbers exist so that a device node can name a driver. nk has
	 * no device nodes and one disk. */
	(void)name;
	(void)probe;
	return major ? 0 : 259;
}

void unregister_blkdev(unsigned int major, const char *name)
{
	(void)major;
	(void)name;
}

bool set_capacity_and_notify(struct gendisk *disk, sector_t size)
{
	(void)disk;
	nk_set_capacity(size);
	/* "did the size change" -- nk is told once, at probe. */
	return false;
}

/* Queue limits are advisory to a layer nk does not have. */
int queue_limits_commit_update_frozen(struct request_queue *q,
				      struct queue_limits *lim)
{
	if (q && lim)
		q->limits = *lim;
	return 0;
}

unsigned int blk_mq_num_possible_queues(unsigned int max_queues)
{
	(void)max_queues;
	return 1;
}

void blk_mq_map_queues(struct blk_mq_queue_map *qmap)
{
	if (qmap && qmap->mq_map)
		qmap->mq_map[0] = 0;
}

void blk_mq_map_hw_queues(struct blk_mq_queue_map *qmap, struct device *dev,
			  unsigned int offset)
{
	(void)dev;
	(void)offset;
	blk_mq_map_queues(qmap);
}

/*
 * The queue-state calls. Every one is a no-op because they all regulate the
 * flow of requests from a layer above the driver, and here there is no such
 * layer -- nk_blk_read is the only source and it submits one at a time.
 */
void blk_mq_start_request(struct request *rq) { (void)rq; }
void blk_mq_stop_hw_queue(struct blk_mq_hw_ctx *h) { (void)h; }
void blk_mq_start_stopped_hw_queues(struct request_queue *q, bool async)
{ (void)q; (void)async; }
void blk_mq_quiesce_queue_nowait(struct request_queue *q) { (void)q; }
void blk_mq_unquiesce_queue(struct request_queue *q) { (void)q; }
void blk_mq_freeze_queue_nomemsave(struct request_queue *q) { (void)q; }
void blk_mq_unfreeze_queue_nomemrestore(struct request_queue *q) { (void)q; }
void blk_mq_requeue_request(struct request *rq, bool kick) { (void)rq; (void)kick; }

/*
 * Completion. The driver calls blk_mq_complete_request from its interrupt
 * handler; in Linux that may bounce to another CPU or a softirq, which is
 * what blk_mq_complete_request_remote is for. nk has one CPU and calls
 * straight through -- ops->complete is virtblk_request_done, which ends in
 * blk_mq_end_request below.
 */
bool blk_mq_complete_request_remote(struct request *rq)
{
	(void)rq;
	return false;
}

void blk_mq_complete_request(struct request *rq)
{
	if (ops && ops->complete)
		ops->complete(rq);
}

void blk_mq_end_request(struct request *rq, blk_status_t error)
{
	if (rq == inflight) {
		inflight_status = error;
		/* Last, and after the status: the waiter spins on this and
		 * must not see it set before the value it is waiting for. */
		inflight_done = 1;
	}
}

void blk_mq_end_request_batch(struct io_comp_batch *iob)
{
	(void)iob;
}

int blk_status_to_errno(blk_status_t status)
{
	return status == BLK_STS_OK ? 0 : -EIO;
}

/*
 * sg_init_table and sg_init_one are Linux's own now -- lib/scatterlist.c is
 * in the port. Only the chained-table allocator is here, because Linux's
 * lives in lib/sg_pool.c on top of mempools, and nk has none.
 *
 * nk builds requests of exactly one contiguous buffer, so the general case --
 * a chain assembled from several bios -- cannot arise, and pretending to
 * handle it would be untested code that looks reassuring.
 *
 * sg_set_buf goes through virt_to_page, and it round-trips exactly here
 * because nk is identity mapped with kimage_voffset at zero; see mm.c. The
 * struct page in between points into a vmemmap nk never allocated, and
 * nothing dereferences it.
 */
int sg_alloc_table_chained(struct sg_table *table, int nents,
			   struct scatterlist *first_chunk, unsigned int nents_first_chunk)
{
	if (nents > (int)nents_first_chunk)
		return -EINVAL;
	sg_init_table(first_chunk, nents);
	table->sgl = first_chunk;
	table->nents = nents;
	table->orig_nents = nents;
	return 0;
}

void sg_free_table_chained(struct sg_table *table, unsigned int nents_first_chunk)
{
	(void)table;
	(void)nents_first_chunk;
}

int __blk_rq_map_sg(struct request *rq, struct scatterlist *sglist,
		    struct scatterlist **last_sg)
{
	if (rq != inflight || !inflight_buf)
		return 0;
	sg_set_buf(sglist, inflight_buf, inflight_len);
	sg_mark_end(sglist);
	if (last_sg)
		*last_sg = sglist;
	return 1;
}

/* Not reached: nk issues no passthrough requests. Present so that the
 * driver's ioctl path links. */
struct request *blk_mq_alloc_request(struct request_queue *q, blk_opf_t opf,
				     blk_mq_req_flags_t flags)
{
	(void)q; (void)opf; (void)flags;
	return ERR_PTR(-ENOMEM);
}

void blk_mq_free_request(struct request *rq) { (void)rq; }

int blk_rq_map_kern(struct request *rq, void *kbuf, unsigned int len, gfp_t gfp)
{
	(void)rq; (void)kbuf; (void)len; (void)gfp;
	return -EINVAL;
}

blk_status_t blk_execute_rq(struct request *rq, bool at_head)
{
	(void)rq; (void)at_head;
	return BLK_STS_IOERR;
}

/* ---------------------------------------------------------------- nk --- */

/*
 * Read one run of sectors, synchronously.
 *
 * Everything between queue_rq and the completion is the unmodified driver:
 * it builds the virtio header, maps the scatterlist, chains the descriptors,
 * writes the notify register, and its own interrupt handler walks the used
 * ring and completes the request. nk only supplies the request and waits.
 */
int nk_blk_read(unsigned long long sector, void *buf, unsigned int len);
int nk_blk_read(unsigned long long sector, void *buf, unsigned int len)
{
	struct blk_mq_queue_data bd;
	struct request *rq;
	blk_status_t status;
	unsigned long long deadline;

	if (!ops || !the_disk)
		return -ENODEV;

	/* The driver's per-request data lives immediately after the request --
	 * blk_mq_rq_to_pdu is (rq + 1) -- so the two are allocated together
	 * and the driver's cmd_size decides how much follows. */
	rq = nk_alloc(sizeof(*rq) + cmd_size, 8);
	if (!rq)
		return -ENOMEM;
	memset(rq, 0, sizeof(*rq) + cmd_size);

	rq->q = the_disk->queue;
	rq->mq_hctx = &hctx;
	rq->cmd_flags = REQ_OP_READ;
	rq->__sector = sector;
	rq->__data_len = len;
	rq->nr_phys_segments = 1;

	inflight = rq;
	inflight_buf = buf;
	inflight_len = len;
	inflight_done = 0;
	inflight_status = BLK_STS_OK;

	bd.rq = rq;
	bd.last = true;

	status = ops->queue_rq(&hctx, &bd);
	if (status != BLK_STS_OK) {
		inflight = NULL;
		nk_free(rq);
		return -EIO;
	}

	/*
	 * A deadline rather than an unbounded wait. A device that never
	 * answers is a real possibility -- a wrong interrupt number produces
	 * exactly that -- and a kernel that hangs says far less than one that
	 * reports a timeout.
	 */
	deadline = nk_ticks() + nk_hz() * 5;
	while (!inflight_done) {
		if (nk_ticks() > deadline) {
			inflight = NULL;
			return -ETIMEDOUT;
		}
		nk_yield();
	}

	status = inflight_status;
	inflight = NULL;
	nk_free(rq);
	return status == BLK_STS_OK ? 0 : -EIO;
}
