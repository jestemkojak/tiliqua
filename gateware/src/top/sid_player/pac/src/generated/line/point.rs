#[doc = "Register `point` reader"]
pub type R = crate::R<POINT_SPEC>;
#[doc = "Register `point` writer"]
pub type W = crate::W<POINT_SPEC>;
#[doc = "Field `x` writer - x field"]
pub type X_W<'a, REG> = crate::FieldWriter<'a, REG, 12, u16>;
#[doc = "Field `y` writer - y field"]
pub type Y_W<'a, REG> = crate::FieldWriter<'a, REG, 11, u16>;
#[doc = "Field `pixel` writer - pixel field"]
pub type PIXEL_W<'a, REG> = crate::FieldWriter<'a, REG, 8>;
#[doc = "Field `cmd` writer - cmd field"]
pub type CMD_W<'a, REG> = crate::BitWriter<'a, REG>;
impl W {
    #[doc = "Bits 0:11 - x field"]
    #[inline(always)]
    pub fn x(&mut self) -> X_W<'_, POINT_SPEC> {
        X_W::new(self, 0)
    }
    #[doc = "Bits 12:22 - y field"]
    #[inline(always)]
    pub fn y(&mut self) -> Y_W<'_, POINT_SPEC> {
        Y_W::new(self, 12)
    }
    #[doc = "Bits 23:30 - pixel field"]
    #[inline(always)]
    pub fn pixel(&mut self) -> PIXEL_W<'_, POINT_SPEC> {
        PIXEL_W::new(self, 23)
    }
    #[doc = "Bit 31 - cmd field"]
    #[inline(always)]
    pub fn cmd(&mut self) -> CMD_W<'_, POINT_SPEC> {
        CMD_W::new(self, 31)
    }
}
#[doc = "A CSR register. Parameters ---------- fields : :class:`dict` or :class:`list` or :class:`Field` Collection of register fields. If ``None`` (default), a dict is populated from Python :term:`variable annotations <python:variable annotations>`. ``fields`` is used to create a :class:`FieldActionMap`, :class:`FieldActionArray`, or :class:`FieldAction`, depending on its type (dict, list, or Field). Interface attributes -------------------- element : :class:`Element` Interface between this register and a CSR bus primitive. Attributes ---------- field : :class:`FieldActionMap` or :class:`FieldActionArray` or :class:`FieldAction` Collection of field instances. f : :class:`FieldActionMap` or :class:`FieldActionArray` or :class:`FieldAction` Shorthand for :attr:`Register.field`. Raises ------ :exc:`TypeError` If ``fields`` is neither ``None``, a :class:`dict`, a :class:`list`, or a :class:`Field`. :exc:`ValueError` If ``fields`` is not ``None`` and at least one variable annotation is a :class:`Field`. :exc:`ValueError` If ``element.access`` is not readable and at least one field is readable. :exc:`ValueError` If ``element.access`` is not writable and at least one field is writable.\n\nYou can [`read`](crate::Reg::read) this register and get [`point::R`](R). You can [`reset`](crate::Reg::reset), [`write`](crate::Reg::write), [`write_with_zero`](crate::Reg::write_with_zero) this register using [`point::W`](W). You can also [`modify`](crate::Reg::modify) this register. See [API](https://docs.rs/svd2rust/#read--modify--write-api)."]
pub struct POINT_SPEC;
impl crate::RegisterSpec for POINT_SPEC {
    type Ux = u32;
}
#[doc = "`read()` method returns [`point::R`](R) reader structure"]
impl crate::Readable for POINT_SPEC {}
#[doc = "`write(|w| ..)` method takes [`point::W`](W) writer structure"]
impl crate::Writable for POINT_SPEC {
    type Safety = crate::Unsafe;
}
#[doc = "`reset()` method sets point to value 0"]
impl crate::Resettable for POINT_SPEC {}
